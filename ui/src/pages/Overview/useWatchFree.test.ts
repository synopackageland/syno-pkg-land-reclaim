import { setBackend, type ApiError } from '@/api'
import type { WatchFreeDocument } from '@/types'
import doc from '../../../fixtures/watch_free.sample/volume1.json'
import { useWatchFree } from './useWatchFree'

function backendFreeing(step: number) {
  let free = 1000
  const calls: unknown[] = []
  setBackend(async (_method, params) => {
    calls.push(params)
    const answer = structuredClone(doc) as WatchFreeDocument
    answer.samples = [
      { ...answer.samples[0], free_bytes: free, pending_deletion_roots: 2 },
      { ...answer.samples[1], free_bytes: (free += step), pending_deletion_roots: free > 1000 + 2 * step ? 0 : 1 },
    ]
    return answer as never
  })
  return calls
}

describe('useWatchFree', () => {
  beforeEach(() => vi.useFakeTimers())
  afterEach(() => vi.useRealTimers())

  it('samples every 2 s for 60 s and reports the change', async () => {
    const calls = backendFreeing(10)
    const watch = useWatchFree(() => '/volume1')
    const done = watch.start()
    await vi.advanceTimersByTimeAsync(61000)
    await done
    expect(watch.running.value).toBe(false)
    expect(calls).toHaveLength(30)
    expect(calls[0]).toEqual({ volume: '/volume1' })
    expect(watch.points.value[0]).toMatchObject({ x: 0, free: 1000, pending: 2 })
    expect(watch.delta.value).toBe(300)
    expect(watch.pending.value).toEqual({ first: 2, last: 0 })
  })

  it('keeps the samples when watching 60 s more', async () => {
    backendFreeing(1)
    const watch = useWatchFree(() => '/volume1')
    void watch.start()
    await vi.advanceTimersByTimeAsync(61000)
    void watch.start()
    await vi.advanceTimersByTimeAsync(61000)
    expect(watch.points.value.at(-1)!.x).toBeGreaterThanOrEqual(118000)
    expect(watch.delta.value).toBe(60)
  })

  it('stops on request and on errors', async () => {
    const calls = backendFreeing(1)
    const watch = useWatchFree(() => '/volume1')
    void watch.start()
    await vi.advanceTimersByTimeAsync(4100)
    watch.stop()
    await vi.advanceTimersByTimeAsync(10000)
    expect(calls).toHaveLength(3)

    setBackend(() => Promise.reject(Object.assign(new Error('x'), { code: 'cli_error' }) as ApiError))
    await watch.start()
    expect(watch.error.value).not.toBeNull()
    expect(watch.running.value).toBe(false)
  })
})
