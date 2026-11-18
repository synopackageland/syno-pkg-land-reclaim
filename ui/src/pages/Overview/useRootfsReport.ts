import { runInBackground } from '@/api/planning'
// `rootfs.report` once per Overview mount:
// the System partition view and the Full actions row read the same answer, so
// selecting the card never starts a second walk of `/`. The CLI keeps the last
// report: it shows at once, and a walk in the background replaces it.
import { getCurrentScope, onScopeDispose, ref, watch } from 'vue'
import { api, ApiError } from '@/api'
import { answersVersion } from '@/stores/answers'
import type { RootfsReportDocument } from '@/types'

/** The oldest saved report shown (the CLI's limit): its age is on screen. */
export const SAVED_MAX_AGE_S = 30 * 24 * 60 * 60
/** A saved report older than this is shown while a walk checks again. */
export const REFRESH_AFTER_S = 5 * 60

const asApiError = (error: unknown) => (error instanceof ApiError ? error : new ApiError('unknown'))
const ageS = (report: RootfsReportDocument) => (Date.now() - Date.parse(report.generated_at)) / 1000

export function useRootfsReport() {
  const report = ref<RootfsReportDocument | null>(null)
  /** Nothing to show yet: the first walk is under way. */
  const loading = ref(false)
  /** A walk is checking again while `report` stays on screen. */
  const refreshing = ref(false)
  const error = ref<ApiError | null>(null)
  const controller = new AbortController()
  if (getCurrentScope()) onScopeDispose(() => controller.abort())

  let generation = 0
  let running: Promise<void> | null = null
  let again = false

  /** Once; `force` walks `/` again, keeping the shown report until it is done.
   * A refresh asked for during a read runs after it; both wait for it. */
  function load({ force = false } = {}): Promise<void> {
    // A saved result is already loading; selecting its card must not start a scan.
    if (loading.value && !running) return Promise.resolve()
    if (running) {
      again ||= force
      return running
    }
    if (report.value && !force) return Promise.resolve()
    running = (async () => {
      let fresh = force
      do {
        again = false
        await read(fresh)
        fresh = true
      } while (again)
    })().finally(() => {
      running = null
      // An old saved report stays on screen while a walk checks again.
      const shown = report.value
      if (!force && shown && !error.value && ageS(shown) > REFRESH_AFTER_S) void load({ force: true })
    })
    return running
  }

  async function read(fresh: boolean) {
    const version = ++generation
    const behind = !!report.value
    loading.value = !behind
    refreshing.value = behind
    error.value = null
    try {
      const response = await runInBackground('rootfs.report', { max_age_s: fresh ? 0 : SAVED_MAX_AGE_S }, { signal: controller.signal })
      if (version !== generation) return
      if (response.completion === 'failed') {
        error.value = new ApiError('cli_error', undefined, { rootfs_code: response.errors[0]?.code })
        return
      }
      report.value = response
    } catch (failure) {
      if (version === generation) error.value = asApiError(failure)
    } finally {
      if (version === generation) {
        loading.value = false
        refreshing.value = false
      }
    }
  }

  /** Open the exact finished report without starting a second scan. */
  async function loadResult(jobId: string) {
    const version = ++generation
    report.value = null
    error.value = null
    loading.value = true
    refreshing.value = false
    try {
      const response = await api('job.result', { job_id: jobId })
      if (version !== generation) return
      if (response.command !== 'rootfs' || !['complete', 'partial'].includes(response.completion)) throw new ApiError('cli_error')
      report.value = response as RootfsReportDocument
    } catch (failure) {
      if (version === generation) error.value = asApiError(failure)
    } finally {
      if (version === generation) loading.value = false
    }
  }

  // A rootfs apply moved bytes, and the CLI dropped its saved report: walk again (P0-3).
  watch(answersVersion, () => {
    if (!report.value && !error.value) return
    void load({ force: true })
  })

  return { report, loading, refreshing, error, load, loadResult }
}
