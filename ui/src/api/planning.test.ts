import { vi } from 'vitest'
import { setBackend, ApiError } from '@/api'
import type { Backend } from './methods'
import { runInBackground } from './planning'
import { fixtureBackend } from './fixtures'

describe('asynchronous planning', () => {
  it('keeps main Deep report/plan fields through the asynchronous fixture API', async () => {
    setBackend(fixtureBackend({ scenario: 'sysvol_deep_short' }))
    const report = await runInBackground('rootfs.report', {})
    if (!('cleanable' in report) || !report.cleanable) throw new Error('Missing report')
    expect(report.cleanable.deep?.by_category.map(row => row.category)).toEqual(expect.arrayContaining(['tool_cache', 'backup_installer']))
    expect(report.cleanable.deep?.tools).toContain('delete_log')
    setBackend(fixtureBackend({ scenario: 'sysvol_deep_short' }))
    const plan = await runInBackground('rootfs.plan', { to: '/volume1/backup', mode: 'deep' })
    if (!('moves' in plan) || !plan.moves || !plan.tools) throw new Error('Missing plan')
    expect(plan.moves.by_category.map(row => row.category)).toEqual(expect.arrayContaining(['tool_cache', 'backup_installer']))
    expect(plan.tools.some(row => row.tool === 'delete_log')).toBe(true)
  })
  it('follows rootfs report by persisted ID without waiting in start', async () => {
    const calls: string[] = []
    setBackend((async method => {
      calls.push(method)
      if (method === 'rootfs.report') return { command: 'detach', job_id: 'report-1' }
      if (method === 'jobs.list') return { jobs: [{ job_id: 'report-1', kind: 'rootfs-report', state: 'completed' }] }
      return { command: 'rootfs', completion: 'complete' }
    }) as Backend)
    expect(await runInBackground('rootfs.report', {})).toMatchObject({ command: 'rootfs' })
    expect(calls).toEqual(['rootfs.report', 'jobs.list', 'job.result'])
  })
  it('answers a saved rootfs report the CLI returns at once, with no job to follow', async () => {
    const calls: string[] = []
    setBackend((async method => {
      calls.push(method)
      return { command: 'rootfs', completion: 'partial' }
    }) as Backend)
    expect(await runInBackground('rootfs.report', { max_age_s: 600 })).toMatchObject({ command: 'rootfs', completion: 'partial' })
    expect(calls).toEqual(['rootfs.report'])
  })
  afterEach(() => vi.useRealTimers())
  it('starts exactly once and fetches the original job after running/queued polls', async () => {
    vi.useFakeTimers()
    let polls = 0
    const calls: [string, unknown][] = []
    setBackend((async (method, params) => {
      calls.push([method, params])
      if (method === 'cleanup.plan') return { command: 'detach', job_id: 'plan-123' }
      if (method === 'jobs.list') return { jobs: [{ job_id: 'other', state: 'completed' }, { job_id: 'plan-123', kind: 'cleanup-plan', state: ++polls > 1 ? 'completed' : 'running' }] }
      if (method === 'job.result') return { completion: 'complete', plan: { plan_id: 'cleanup-aa-bb' } }
      throw Error(method)
    }) as Backend)
    const accepted = vi.fn()
    const result = runInBackground('cleanup.plan', { paths: ['/volume1/data'] }, { onAccepted: accepted })
    await vi.advanceTimersByTimeAsync(2000)
    expect(await result).toMatchObject({ plan: { plan_id: 'cleanup-aa-bb' } })
    expect(accepted).toHaveBeenCalledWith('plan-123')
    expect(calls.filter(([method]) => method === 'cleanup.plan')).toHaveLength(1)
    expect(calls.find(([method]) => method === 'job.result')?.[1]).toEqual({ job_id: 'plan-123' })
  })
  it('does not invent a job, retry a lost start, or fetch a cancelled result', async () => {
    setBackend((async () => { throw new ApiError('timeout') }) as Backend)
    await expect(runInBackground('cleanup.plan', { paths: ['/volume1/data'] })).rejects.toMatchObject({ code: 'timeout' })
    const result = vi.fn()
    setBackend((async method => {
      if (method === 'rootfs.plan') return { command: 'detach', job_id: 'plan-1' }
      if (method === 'jobs.list') return { jobs: [{ job_id: 'plan-1', kind: 'rootfs-plan', state: 'cancelled' }] }
      result()
      return {}
    }) as Backend)
    await expect(runInBackground('rootfs.plan', { to: '/volume1/backup' })).rejects.toBeInstanceOf(ApiError)
    expect(result).not.toHaveBeenCalled()
  })
  it('aborts monitoring without dispatching cancellation or restarting the job', async () => {
    const controller = new AbortController()
    const methods: string[] = []
    setBackend((async method => {
      methods.push(method)
      if (method === 'cleanup.plan') { controller.abort(); return { command: 'detach', job_id: 'plan-2' } }
      throw Error(method)
    }) as Backend)
    await expect(runInBackground('cleanup.plan', { paths: ['/volume1/a'] }, { signal: controller.signal })).rejects.toBeDefined()
    expect(methods).toEqual(['cleanup.plan'])
  })
})
