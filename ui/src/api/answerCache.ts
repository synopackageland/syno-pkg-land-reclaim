// Bounded cache of report answers. A report is immutable for one `answersVersion`, so the
// same method and params can be answered from the last result while no scan,
// cleanup or rootfs apply has changed the reports. The cache keeps the newest
// `ANSWER_CACHE_LIMIT` answers, least recently read first, and drops all of
// them when `answersVersion` rises.
import { answerKey, answersVersion } from '@/stores/answers'

/** How many answers stay in memory: several levels of drill-down and share pages. */
export const ANSWER_CACHE_LIMIT = 100

type Entry = { answer: Promise<unknown> }

let version = answersVersion.value
const entries = new Map<string, Entry>()

/** Forget every cached answer; the next read asks the backend again. */
export function clearAnswers() {
  entries.clear()
  version = answersVersion.value
}

function currentEntries() {
  // answersVersion rises when the reports stop describing the volume: no
  // answer keyed on the old version survives (design review 2026-10-01 §7.3).
  if (version !== answersVersion.value) clearAnswers()
  return entries
}

/**
 * `method` with `params` answered from the cache when it was answered at the
 * current `answersVersion`, else from `load()`. A rejection is not kept, so a
 * busy or failed answer is asked again next time.
 */
export function cachedAnswer<T>(method: string, params: Record<string, unknown>, load: () => Promise<T>): Promise<T> {
  const cache = currentEntries()
  const key = answerKey(method, params)
  const hit = cache.get(key)
  if (hit) {
    cache.delete(key)
    cache.set(key, hit)
    return hit.answer as Promise<T>
  }
  const answer = load().catch(error => {
    if (cache.get(key)?.answer === answer) cache.delete(key)
    throw error
  })
  cache.set(key, { answer })
  while (cache.size > ANSWER_CACHE_LIMIT) {
    const oldest = cache.keys().next().value
    if (oldest === undefined) break
    cache.delete(oldest)
  }
  return answer
}
