// UI follows the same persisted product jobs exposed to ctl. The start call
// returns immediately; polling never repeats a start after a lost response.
import { api, ApiError } from '@/api'
import type { ParamsOf } from './methods'
import type { CleanupPlanDocument, RootfsPlanResultDocument, RootfsDocument } from '@/types'

type BackgroundMethod = 'cleanup.plan' | 'rootfs.plan' | 'rootfs.report'
type BackgroundResult<M extends BackgroundMethod> = M extends 'cleanup.plan' ? CleanupPlanDocument : M extends 'rootfs.plan' ? RootfsPlanResultDocument : RootfsDocument

/**
 * The job's progress at one check: what its current `step` (a cleanup plan
 * checks kept copies, then items) has done of its total.
 */
export type PlanningProgress = { done: number; total: number | null; step?: 'kept' | 'items' }
/** `onProgress`: the job's progress at each check, such as the items a cleanup plan checked. */
type FollowOptions = { signal?: AbortSignal; onAccepted?: (id: string) => void; onProgress?: (progress: PlanningProgress) => void }
function check(signal?: AbortSignal) {
  if (signal?.aborted) throw new DOMException('Monitoring stopped', 'AbortError')
}
function delay(signal?: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(new DOMException('Monitoring stopped', 'AbortError')) }
    const timer = setTimeout(() => { signal?.removeEventListener('abort', abort); resolve() }, 1000)
    signal?.addEventListener('abort', abort, { once: true })
    if (signal?.aborted) abort()
  })
}

export async function runInBackground<M extends BackgroundMethod>(method: M, params: ParamsOf<M>, options: FollowOptions = {}): Promise<BackgroundResult<M>> {
  check(options.signal)
  const accepted = await api(method, params)
  // A saved rootfs report within max_age_s comes back at once, with no job.
  if (method === 'rootfs.report' && accepted.command === 'rootfs') return accepted as BackgroundResult<M>
  const id = accepted.job_id
  if (accepted.command !== 'detach' || !id) throw new ApiError('cli_error', undefined, { job_id: id })
  options.onAccepted?.(id)
  for (;;) {
    check(options.signal)
    const { jobs } = await api('jobs.list', {})
    check(options.signal)
    const job = jobs.find(job => job.job_id === id)
    const kind = method.replace('.', '-')
    if (!job || job.kind !== kind) throw new ApiError('cli_error', undefined, { job_id: id })
    if (['completed', 'partial', 'failed'].includes(job.state)) {
      return await api('job.result', { job_id: id }) as BackgroundResult<M>
    }
    if (!['queued', 'running', 'cancelling'].includes(job.state)) throw new ApiError('cli_error', undefined, { job_id: id })
    if (job.progress) options.onProgress?.({ done: job.progress.units_done, total: job.progress.units_total, ...(job.progress.step ? { step: job.progress.step } : {}) })
    await delay(options.signal)
  }
}
