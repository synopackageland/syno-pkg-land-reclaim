// The current time as a ref that ticks, so time-based states (a stalled
// progress, a long pause) change while the page stays open.
import { onUnmounted, ref } from 'vue'

export function useNow(intervalMs = 5000) {
  const now = ref(Date.now())
  const timer = setInterval(() => (now.value = Date.now()), intervalMs)
  onUnmounted(() => clearInterval(timer))
  return now
}
