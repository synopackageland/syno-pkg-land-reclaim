// The cleanup list's exact total: simulate.get answers for the
// whole set at once, so shared extents and folders deleted together count
// once. Asked in the background after the selection settles; until it
// answers, or when it cannot (no report of exactly this set), the page keeps
// the lower bound it already shows.
import { getCurrentScope, onScopeDispose, ref, watch } from 'vue'
import { api } from '@/api'
import type { BasketItem } from '@/stores/basket'

const SETTLE_MS = 600

export function useBasketEstimate(items: () => BasketItem[]) {
  const exact = ref<number | null>(null)
  const pending = ref(false)
  let timer: ReturnType<typeof setTimeout> | undefined
  let request: AbortController | null = null

  function cancel() {
    clearTimeout(timer)
    request?.abort()
    request = null
  }

  async function ask(selection: BasketItem[]) {
    const controller = new AbortController()
    request = controller
    try {
      const paths = selection.map(item => item.path_hex ? { path: item.path, path_hex: item.path_hex } : item.path)
      const answer = await api('simulate.get', { paths }, { signal: controller.signal })
      if (controller.signal.aborted) return
      const values = answer.results.map(result => result.combined.reclaimable)
      exact.value = answer.errors.length || !values.length || values.some(value => value == null)
        ? null
        : values.reduce<number>((sum, value) => sum + (value ?? 0), 0)
    } catch {
      if (controller.signal.aborted) return
      exact.value = null
    } finally {
      if (request === controller) {
        request = null
        pending.value = false
      }
    }
  }

  watch(items, selection => {
    cancel()
    exact.value = null
    pending.value = selection.length > 0
    if (selection.length) timer = setTimeout(() => void ask(selection), SETTLE_MS)
  }, { immediate: true })

  if (getCurrentScope()) onScopeDispose(cancel)

  return { exact, pending }
}
