// Overview.get per volume (for the cards) and why_not_freed.get for the
// selected one, kept per mount path. The two calls answer independently: the
// hero and cards are published as soon as overview.get resolves, while the
// optional why explanations enrich the action list afterwards.
// An answer is only reused while the report version and the request params it
// was fetched for are unchanged: a new scan, or a cleanup
// that changes what the reports describe, raises answersVersion and makes it
// miss.
import { reactive } from 'vue'
import { api, ApiError } from '@/api'
import { answerKey, answersVersion } from '@/stores/answers'
import type { OverviewDocument, WhyNotFreedDocument } from '@/types'

export type VolumeAnswer = {
  /** The answersVersion this answer was fetched for; an older one must be refetched. */
  version: number
  /** The request params (the cache key); different params need a new answer. */
  requestKey: string
  /** True until overview.get settles: the hero and the cards are not final yet. */
  overviewLoading: boolean
  /** True while why_not_freed.get is in flight: the reasons are not known yet. */
  whyPending: boolean
  /** True when the optional why_not_freed.get failed: the reasons stay unknown. */
  whyFailed: boolean
  overview: OverviewDocument | null
  why: WhyNotFreedDocument | null
  error: ApiError | null
}

const asApiError = (e: unknown) => (e instanceof ApiError ? e : new ApiError('unknown'))

export function useOverviewData() {
  const answers = reactive<Record<string, VolumeAnswer>>({})
  // One counter per volume: a late reply from a superseded read must not touch
  // an answer that a newer read (or a version change) has already replaced.
  const readTokens = new Map<string, number>()

  async function load(volume: string, { why = false, force = false } = {}) {
    const params = { volume, ...(why ? { why } : {}) }
    const requestKey = answerKey('overview.get', params)
    const known = answers[volume]
    const current = known?.version === answersVersion.value && known.requestKey === requestKey
    const hasReport = Boolean(known?.overview?.results.length)
    const inFlight = Boolean(known && known.version === answersVersion.value && (known.overviewLoading || known.whyPending))
    if (known && !force && (inFlight || (current && hasReport && (!why || known.why)))) return

    const token = (readTokens.get(volume) ?? 0) + 1
    readTokens.set(volume, token)
    answers[volume] = {
      version: answersVersion.value,
      requestKey,
      overviewLoading: true,
      whyPending: why,
      whyFailed: false,
      overview: known?.overview ?? null,
      why: known?.why ?? null,
      error: null,
    }
    const answer = answers[volume]
    const isCurrent = () => readTokens.get(volume) === token && answers[volume] === answer && answer.version === answersVersion.value

    const overview = api('overview.get', { volume }).then(
      value => {
        if (!isCurrent()) return
        answer.overview = value
        answer.error = null
        answer.overviewLoading = false
      },
      reason => {
        if (!isCurrent()) return
        answer.error = asApiError(reason)
        answer.overviewLoading = false
      },
    )
    // why-not-freed is optional: it resolves on its own and its failure never
    // hides the overview.
    const reasons = why
      ? api('why_not_freed.get', { volume }).then(
          value => {
            if (!isCurrent()) return
            answer.why = value
            answer.whyFailed = false
            answer.whyPending = false
          },
          () => {
            if (!isCurrent()) return
            answer.whyFailed = true
            answer.whyPending = false
          },
        )
      : Promise.resolve()

    await Promise.all([overview, reasons])
  }

  return { answers, load }
}
