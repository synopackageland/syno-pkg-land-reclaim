import { afterEach, describe, expect, it, vi } from 'vitest'
import { dsmWebApi } from './dsmWebApi'

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })

describe('DSM window WebAPI adapter', () => {
  it('returns the window data without a token, envelope or fetch', async () => {
    const data = { snapshots: [{ lock: true }] }
    const ctx = { webApi: vi.fn(async () => data) }
    const fetcher = vi.fn(); vi.stubGlobal('fetch', fetcher)
    const params = { name: 'share01', additional: ['lock', 'worm_lock'] }
    expect(await dsmWebApi(ctx, 'SYNO.Core.Share.Snapshot', 'list', 2, params)).toBe(data)
    expect(ctx.webApi).toHaveBeenCalledExactlyOnceWith('SYNO.Core.Share.Snapshot', 'list', 2, params)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each(['dsm_401', 'dsm_105', 'dsm_error'])('preserves the SDK refusal %s', async code => {
    await expect(dsmWebApi({ webApi: async () => { throw { code } } }, 'api', 'list', 1, {}))
      .rejects.toEqual({ code })
  })

  it('fails closed without a window API, even when fetch is available', async () => {
    const fetcher = vi.fn(); vi.stubGlobal('fetch', fetcher)
    await expect(dsmWebApi({}, 'api', 'list', 1, {})).rejects.toMatchObject({ code: 'dsm_unavailable' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('does not send an already canceled read', async () => {
    const ctx = { webApi: vi.fn() }
    const controller = new AbortController(); controller.abort()
    await expect(dsmWebApi(ctx, 'api', 'list', 1, {}, controller.signal)).rejects.toMatchObject({ name: 'AbortError' })
    expect(ctx.webApi).not.toHaveBeenCalled()
  })

  it('stops waiting on cancellation while a late SDK reply stays handled', async () => {
    let reject!: (error: unknown) => void
    const ctx = { webApi: () => new Promise((_, fail) => { reject = fail }) }
    const controller = new AbortController()
    const pending = dsmWebApi(ctx, 'api', 'list', 1, {}, controller.signal)
    const assertion = expect(pending).rejects.toMatchObject({ name: 'AbortError' })
    controller.abort()
    await assertion
    reject({ code: 'dsm_401' })
  })

  it('keeps waiting for a slow window read and returns its data after a minute', async () => {
    vi.useFakeTimers()
    const data = { snapshots: [{ lock: true }] }
    const ctx = { webApi: () => new Promise(resolve => setTimeout(() => resolve(data), 60_000)) }
    let settled = false
    const pending = dsmWebApi(ctx, 'api', 'list', 1, {}).finally(() => { settled = true })
    const assertion = expect(pending).resolves.toBe(data)
    await vi.advanceTimersByTimeAsync(59_999)
    expect(settled).toBe(false)
    await vi.advanceTimersByTimeAsync(1)
    await assertion
  })

  it('cancels a slow read after a minute without leaving an abort listener', async () => {
    vi.useFakeTimers()
    const controller = new AbortController()
    const remove = vi.spyOn(controller.signal, 'removeEventListener')
    const pending = dsmWebApi({ webApi: () => new Promise(() => {}) }, 'api', 'list', 1, {}, controller.signal)
    const assertion = expect(pending).rejects.toMatchObject({ name: 'AbortError' })
    await vi.advanceTimersByTimeAsync(60_000)
    controller.abort()
    await assertion
    expect(remove).toHaveBeenCalledWith('abort', expect.any(Function))
  })
})
