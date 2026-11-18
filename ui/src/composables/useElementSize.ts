import { onBeforeUnmount, onMounted, ref, type Ref } from 'vue'

export type ElementSize = { width: number; height: number }

/** The element's content box, rounded; `initial` until it is first laid out with a size. */
export function useElementSize(target: Ref<Element | null>, initial: ElementSize) {
  const size = ref(initial)
  let observer: ResizeObserver | null = null

  onMounted(() => {
    if (!target.value || typeof ResizeObserver === 'undefined') return
    observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect
      if (!rect) return
      const next = { width: Math.round(rect.width), height: Math.round(rect.height) }
      if (next.width > 0 && next.height > 0) size.value = next
    })
    observer.observe(target.value)
  })

  onBeforeUnmount(() => observer?.disconnect())
  return size
}
