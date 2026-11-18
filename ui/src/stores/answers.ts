// Front-end answer invalidation: an answer is only as
// good as the report and request it was computed from. `answersVersion` rises
// whenever a scan publishes a report, or a cleanup or rootfs apply ends or is
// interrupted, so every cached answer keyed on the old version misses.
import { ref } from 'vue'

export const answersVersion = ref(0)

/** Note that reports may have changed: answers must be fetched again. */
export function invalidateAnswers() {
  answersVersion.value += 1
}

/** The cache key of one answer: report version, method and request params. */
export function answerKey(method: string, params: Record<string, unknown>, version = answersVersion.value) {
  const sorted = Object.entries(params).sort(([a], [b]) => a.localeCompare(b))
  return `${version}|${method}|${JSON.stringify(sorted)}`
}
