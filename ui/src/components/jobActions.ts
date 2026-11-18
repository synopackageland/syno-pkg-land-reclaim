// Pause, cancel, resume and mode changes of a scan job, shared by the
// job strip and the Scanning page. The end state always comes from jobs.list
// (F5), so every action polls again when it returns.
import { computed, onScopeDispose, ref, watch } from 'vue'
import { api } from '@/api'
import type { ScanMode } from '@/api/methods'
import { code, t } from '@/i18n'
import { apiErrorText } from './states/apiErrorText'
import { authorizeRootWork } from '@/stores/authorization'
import { jobs, pollNow } from '@/stores/jobs'
import type { Job } from '@/types'
import { openJob } from '@/stores/nav'

type Action = 'pause' | 'cancel' | 'resume' | 'changeMode'

/** Outcomes that mean the request was taken (common.schema.json job_request). */
const DONE = ['requested', 'cancelled']

export function useJobActions() {
  /** The action waiting for its answer. */
  const pending = ref<Action | null>(null)
  /** A request the job did not take (too late, finished, …), already localized. */
  const notice = ref('')
  /** Why the last request failed, already localized. */
  const failure = ref('')

  const modeChange = ref<{ id: string; mode: ScanMode } | null>(null)
  const modePending = computed(() => modeChange.value !== null)
  function clearModeChange() {
    modeChange.value = null
  }
  onScopeDispose(clearModeChange)
  watch(() => {
    const job = jobs.list.find(job => job.job_id === modeChange.value?.id)
    return [job?.profile, job?.state, job?.phase, job?.progress?.phase] as const
  }, ([profile, state, phase, progressPhase]) => {
    if (!modeChange.value || !state) return
    if ((profile === 'low' ? 'gentle' : 'fast') === modeChange.value.mode) clearModeChange()
    else if (phase === 'publish' || progressPhase === 'publish') {
      clearModeChange()
      notice.value = t('scan.changeMode.publishing')
    } else if (state !== 'running') {
      clearModeChange()
      notice.value = t('scan.changeMode.unconfirmed')
    }
  })

  /** Runs one request; false when it failed or the user did not authorize it. */
  async function run(action: Action, request: () => Promise<object>) {
    pending.value = action
    notice.value = ''
    failure.value = ''
    try {
      if (!(await authorizeRootWork())) return false
      const answer = await request()
      const outcome = 'outcome' in answer ? String(answer.outcome) : null
      if (outcome === 'too_late') notice.value = t(`scan.${action}.tooLate`)
      else if (outcome && !DONE.includes(outcome)) notice.value = code('outcome', outcome)
      pollNow()
      return true
    } catch (error) {
      failure.value = apiErrorText(error)
      return false
    } finally {
      pending.value = null
    }
  }

  return {
    pending,
    notice,
    failure,
    modePending,
    pause: (job: Job) => run('pause', () => api('job.pause', { job_id: job.job_id })),
    cancel: (job: Job) =>
      run('cancel', async () => {
        const answer = await api('job.cancel', { job_id: job.job_id })
        if (answer.outcome === 'requested') jobs.cancelRequested.push(job.job_id)
        return answer
      }),
    fresh: (job: Job) => run('resume', async () => {
      const answer = await api('job.resume', { job_id: job.job_id, fresh: true })
      if (answer.job_id) openJob({ job_id: answer.job_id, kind: 'scan' })
      return answer
    }),
    resume: (job: Job, mode: ScanMode) => run('resume', () => api('job.resume', { job_id: job.job_id, mode })),
    /** SPEC §4.2 Change mode: the running scan goes on in `mode`, keeping its progress. */
    changeMode: (job: Job, mode: ScanMode) => {
      if (pending.value || modePending.value) return Promise.resolve(false)
      return run('changeMode', async () => {
        const answer = await api('job.mode', { job_id: job.job_id, mode })
        if (answer.outcome === 'requested') {
          modeChange.value = { id: job.job_id, mode }
        }
        return answer
      })
    },
  }
}
