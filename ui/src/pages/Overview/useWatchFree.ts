// Watch free space: one watch_free.sample call
// every 2 s for 60 s, appended client-side.
import { computed, ref } from 'vue'
import { api, ApiError } from '@/api'
import type { WatchFreeDocument } from '@/types'

const INTERVAL_MS = 2000
const WINDOW_MS = 60000

type Sample = WatchFreeDocument['samples'][number]
export type WatchPoint = { x: number; free: number | null; used: number | null; pending: number | null }

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
const toPoint = (x: number, s: Sample): WatchPoint => ({ x, free: s.free_bytes, used: s.data_used_bytes, pending: s.pending_deletion_roots })

export function useWatchFree(volume: () => string) {
  const points = ref<WatchPoint[]>([])
  const running = ref(false)
  const error = ref<ApiError | null>(null)
  let origin = 0
  let run = 0

  async function sampleOnce() {
    const { samples } = await api('watch_free.sample', { volume: volume() })
    if (!points.value.length) {
      origin = Date.now()
      points.value.push(toPoint(0, samples[0]))
    }
    points.value.push(toPoint(Date.now() - origin, samples[samples.length - 1]))
  }

  /** Watch for 60 s more, keeping what was already sampled. */
  async function start() {
    const id = ++run
    const until = Date.now() + WINDOW_MS
    running.value = true
    error.value = null
    try {
      while (id === run && Date.now() < until) {
        const began = Date.now()
        await sampleOnce()
        await sleep(Math.max(0, INTERVAL_MS - (Date.now() - began)))
      }
    } catch (e) {
      error.value = e instanceof ApiError ? e : new ApiError('unknown')
    } finally {
      if (id === run) running.value = false
    }
  }

  function stop() {
    run++
    running.value = false
  }

  function reset() {
    stop()
    points.value = []
    error.value = null
  }

  const first = computed(() => points.value[0])
  const last = computed(() => points.value[points.value.length - 1])
  const delta = computed(() => (first.value?.free != null && last.value?.free != null ? last.value.free - first.value.free : null))
  const pending = computed(() => (first.value ? { first: first.value.pending, last: last.value.pending } : null))
  const used = computed(() => (first.value ? { first: first.value.used, last: last.value.used } : null))

  return { points, running, error, delta, pending, used, start, stop, reset, windowMs: WINDOW_MS }
}
