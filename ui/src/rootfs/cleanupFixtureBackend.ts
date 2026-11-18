// Combines recorded U3 cleanup responses with the simulated U4 job lifecycle.
import { fixtureBackend, scenarioFromUrl } from '@/api/fixtures'
import type { Backend, Method } from '@/api/methods'
import type { Job } from '@/types'
import { rootfsApplyFixtureBackend } from '@/pages/RootfsApply/rootfsApplyFixtureBackend'

const APPLY_METHODS = new Set<string>([
  'jobs.list',
  'job.cancel',
  'job.result',
  'rootfs.apply',
  'rootfs.history',
  'rootfs.report',
])

export function rootfsCleanupFixtureBackend(scenario = scenarioFromUrl()): Backend {
  const cleanup = fixtureBackend({ scenario })
  const applyReportIds = new Set<string>()
  return (async (method: string, params: Record<string, unknown>) => {
    if (method === 'rootfs.report') {
      const accepted = await rootfsApplyFixtureBackend(method, params)
      if ('job_id' in accepted && typeof accepted.job_id === 'string') applyReportIds.add(accepted.job_id)
      return accepted
    }
    if (method === 'jobs.list') {
      const planning = await cleanup('jobs.list', {})
      const apply = await rootfsApplyFixtureBackend(method, params) as { jobs: Job[] }
      return { jobs: [...planning.jobs.filter(job => ['cleanup-plan', 'rootfs-plan', 'rootfs-report'].includes(job.kind)), ...apply.jobs] }
    }
    if (applyReportIds.has(String(params.job_id ?? ''))) {
      return rootfsApplyFixtureBackend(method, params)
    }
    if (String(params.job_id ?? '').startsWith('planning-fixture-')) {
      return cleanup(method as Method, params as never)
    }
    if (APPLY_METHODS.has(method)) {
      return rootfsApplyFixtureBackend(method, params as Record<string, unknown>)
    }
    return cleanup(method as Method, params as never)
  }) as unknown as Backend
}
