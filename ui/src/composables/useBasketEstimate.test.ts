import { nextTick, shallowRef } from 'vue'
import type { BasketItem } from '@/stores/basket'
import { ApiError } from '@/api'
import { useBasketEstimate } from './useBasketEstimate'

const api = vi.hoisted(() => vi.fn())
vi.mock('@/api', async importOriginal => ({ ...(await importOriginal<typeof import('@/api')>()), api }))

function answer(reclaimable: (number | null)[], errors: object[] = []) {
  return { errors, results: reclaimable.map(value => ({ combined: { reclaimable: value } })) }
}

const one: BasketItem = { path: '/volume1/a', reclaimable: 10 }
const two: BasketItem = { path: '/volume1/b', path_hex: '2f766f6c756d65312f62', reclaimable: 20 }

describe('useBasketEstimate', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    api.mockReset()
  })
  afterEach(() => vi.useRealTimers())

  it('asks simulate.get for the whole set once the selection settles, and uses its exact value', async () => {
    api.mockResolvedValue(answer([42]))
    const items = shallowRef<BasketItem[]>([one])
    const estimate = useBasketEstimate(() => items.value)
    items.value = [one, two]
    await nextTick()
    expect(estimate.pending.value).toBe(true)
    expect(api).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(600)
    expect(api).toHaveBeenCalledTimes(1)
    expect(api.mock.calls[0][1]).toEqual({ paths: ['/volume1/a', { path: '/volume1/b', path_hex: '2f766f6c756d65312f62' }] })
    expect(estimate.exact.value).toBe(42)
    expect(estimate.pending.value).toBe(false)
  })

  it('keeps no exact value when the backend has no report of this set', async () => {
    api.mockResolvedValue(answer([], [{ code: 'no_report', path: '/volume1/a' }]))
    const items = shallowRef<BasketItem[]>([one])
    const estimate = useBasketEstimate(() => items.value)
    await vi.advanceTimersByTimeAsync(600)
    expect(estimate.exact.value).toBeNull()
    expect(estimate.pending.value).toBe(false)
  })

  it('keeps no exact value when a part is unknown or the call fails', async () => {
    api.mockResolvedValueOnce(answer([5, null])).mockRejectedValueOnce(new ApiError('busy'))
    const items = shallowRef<BasketItem[]>([one])
    const estimate = useBasketEstimate(() => items.value)
    await vi.advanceTimersByTimeAsync(600)
    expect(estimate.exact.value).toBeNull()
    items.value = [two]
    await vi.advanceTimersByTimeAsync(600)
    expect(estimate.exact.value).toBeNull()
    expect(estimate.pending.value).toBe(false)
  })

  it('drops the previous answer when the selection changes again', async () => {
    let resolveFirst: (value: unknown) => void = () => {}
    api.mockImplementationOnce(() => new Promise(resolve => { resolveFirst = resolve }))
      .mockResolvedValueOnce(answer([7]))
    const items = shallowRef<BasketItem[]>([one])
    const estimate = useBasketEstimate(() => items.value)
    await vi.advanceTimersByTimeAsync(600)
    items.value = [two]
    await nextTick()
    expect((api.mock.calls[0][2] as { signal: AbortSignal }).signal.aborted).toBe(true)
    resolveFirst(answer([99]))
    await vi.advanceTimersByTimeAsync(600)
    expect(estimate.exact.value).toBe(7)
  })

  it('asks nothing for an empty selection', async () => {
    const items = shallowRef<BasketItem[]>([])
    const estimate = useBasketEstimate(() => items.value)
    await vi.advanceTimersByTimeAsync(600)
    expect(api).not.toHaveBeenCalled()
    expect(estimate.pending.value).toBe(false)
    expect(estimate.exact.value).toBeNull()
  })
})
