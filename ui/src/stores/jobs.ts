// Scan jobs (jobs.list), polled at a fixed cadence and
// never while the window is hidden.
import { computed, reactive } from 'vue'
import { api, ApiError } from '@/api'
import type { Job } from '@/types'
import { invalidateAnswers } from './answers'
import { isActiveJob } from './jobStates'
import type { PollTime } from '@/components/scanProgress'
import { loadVolumes, volumes } from './volumes'

export const jobs = reactive({
  list: [] as Job[],
  error: null as ApiError | null,
  /** Set by the Scanning page while it is shown. */
  scanningPageOpen: false,
  /** When the shown list was answered and arrived, for counting time between polls. */
  polled: null as PollTime | null,
  /** Jobs this window asked to cancel: until GAP-19, jobs.list shows them as pausing (F6). */
  cancelRequested: [] as string[],
})

/** The state to show: a pausing job this window asked to cancel is cancelling (F6). */
export function shownState(job: Job): Job['state'] {
  return job.state === 'pausing' && jobs.cancelRequested.includes(job.job_id) ? 'cancelling' : job.state
}

export const activeJobs = (list: Job[]) => list.filter(isActiveJob)
export const runningJobs = computed(() => activeJobs(jobs.list))

export function pollInterval(list: Job[], scanningPageOpen: boolean) {
  if (list.some(job => job.state === 'pausing' || job.state === 'cancelling')) return 1000
  if (scanningPageOpen) return 2000
  return activeJobs(list).length > 0 ? 15000 : 60000
}

/**
 * Reads the jobs. A scan that has ended since the last read changes the volumes'
 * reports, so they are read again; `reloadVolumes: false` is for a caller that
 * reads the volumes alongside and checks `volumesBehindJobs` after both return.
 */
export async function refreshJobs({ reloadVolumes = true }: { reloadVolumes?: boolean } = {}) {
  try {
    const previous = jobs.list
    const answer = await api('jobs.list', {})
    const next = answer.jobs
    const newlyFinished = (job: Job) => {
      const old = previous.find(candidate => candidate.job_id === job.job_id)
      return old == null || isActiveJob(old)
    }
    const endedChangedReports = next.filter(job => newlyFinished(job) && endsWithReportChange(job))
    jobs.list = next
    jobs.polled = { generatedAt: answer.generated_at, receivedAt: Date.now() }
    jobs.error = null
    if (endedChangedReports.length) invalidateAnswers()
    if (reloadVolumes && endedChangedReports.some(job => job.kind === 'scan')) await loadVolumes()
  } catch (error) {
    jobs.error = error instanceof ApiError ? error : new ApiError('unknown')
  }
}

/** Whether a scan ended at or after the volumes on show were read, so they may lack its report. */
export function volumesBehindJobs() {
  const read = Date.parse(volumes.document?.generated_at ?? '')
  return jobs.list.some(job => job.kind === 'scan' && endsWithReportChange(job) && !(Date.parse(job.updated) < read))
}

/** States in which a cleanup or rootfs apply changed something, or stopped part-way. */
const APPLY_ENDED_STATES: readonly Job['state'][] = ['completed', 'partial', 'failed', 'cancelled', 'interrupted']

function endsWithReportChange(job: Job) {
  if (job.kind === 'scan') return job.state === 'completed' || job.state === 'partial'
  return ['cleanup-apply', 'rootfs-apply'].includes(job.kind) && APPLY_ENDED_STATES.includes(job.state)
}

let timer: ReturnType<typeof setTimeout> | undefined

function schedule() {
  clearTimeout(timer)
  if (document.hidden) return
  timer = setTimeout(async () => {
    await refreshJobs()
    schedule()
  }, pollInterval(jobs.list, jobs.scanningPageOpen))
}

function onVisibilityChange() {
  if (document.hidden) clearTimeout(timer)
  else void refreshJobs().then(schedule)
}

/** Keep polling, after loading the jobs now unless the caller just did; returns the stop function. */
export function startJobPolling({ loadNow = true }: { loadNow?: boolean } = {}) {
  document.addEventListener('visibilitychange', onVisibilityChange)
  if (loadNow) void refreshJobs().then(schedule)
  else schedule()
  return () => {
    clearTimeout(timer)
    document.removeEventListener('visibilitychange', onVisibilityChange)
  }
}

/** Poll again now, e.g. right after pause/cancel or when the cadence changes. */
export function pollNow() {
  void refreshJobs().then(schedule)
}
