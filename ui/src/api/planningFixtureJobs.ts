// Development/test adapter for recorded synchronous CLI planning documents.
// Production always uses the App's real persisted asynchronous API.
import type { Backend } from './methods'
import type { Job } from '@/types'

let nextFixtureJob = 0

export function withPlanningFixtureJobs(backend: Backend): Backend {
  const records = new Map<string, { kind: 'cleanup-plan' | 'rootfs-plan' | 'rootfs-report'; document: unknown; state: Job['state'] }>()
  const call: Backend = (method, params, options) => options ? backend(method, params, options) : backend(method, params)
  // The CLI's saved rootfs report, which a call with max_age_s answers at once.
  let savedReport: unknown = null
  return (async (method, params, options) => {
    if (method === 'rootfs.report' && ((params as { max_age_s?: number }).max_age_s ?? 0) > 0 && savedReport) return structuredClone(savedReport)
    if (method === 'cleanup.plan' || method === 'rootfs.plan' || method === 'rootfs.report') {
      const document = await call(method, params as never, options) as unknown as Record<string, unknown>
      if (document.command === 'detach') return document
      if (method === 'rootfs.report' && document.completion !== 'failed') savedReport = document
      const id = `planning-fixture-${++nextFixtureJob}`
      records.set(id, { kind: method.replace('.', '-') as 'cleanup-plan' | 'rootfs-plan' | 'rootfs-report', document, state: document.completion === 'complete' ? 'completed' : 'failed' })
      return { command: 'detach', job_id: id, completion: 'complete', errors: [], warnings: [], launcher: 'none' }
    }
    const id = (params as { job_id?: string }).job_id
    if (id && records.has(id)) {
      const record = records.get(id)!
      if (method === 'job.result') return structuredClone(record.document)
      if (method === 'job.cancel') { record.state = 'cancelled'; return { command: 'cancel', state: 'cancelled', job_id: id } }
    }
    if (method === 'jobs.list' && records.size) {
      let answer: Record<string, unknown>
      try { answer = await call(method, params as never, options) as unknown as Record<string, unknown> }
      catch (error) {
        if ((error as { code?: string }).code !== 'fixture_missing') throw error
        answer = { jobs: [] }
      }
      return { ...answer, jobs: [...((answer.jobs ?? []) as Job[]), ...[...records].map(([job_id, record]) => ({ job_id, kind: record.kind, state: record.state, phase: 'plan', targets: [] }))] }
    }
    return call(method, params as never, options)
  }) as Backend
}
