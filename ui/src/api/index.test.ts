import { afterEach, describe, expect, it } from 'vitest'
import { invalidateAnswers } from '@/stores/answers'
import { api, onRootCallDenied, setBackend, ApiError } from './index'
import type { Backend } from './methods'

/** A backend that counts calls per method and answers with an empty document. */
function countingBackend() {
  const calls: string[] = []
  const backend: Backend = async method => {
    calls.push(method)
    return {} as never
  }
  return { calls, backend }
}

afterEach(() => setBackend(async () => ({}) as never))

describe('api report answers', () => {
  it('answers the same top list from the cache instead of the backend again', async () => {
    const { calls, backend } = countingBackend()
    setBackend(backend)
    const params = { target: '/volume1', kind: 'all' as const, by: 'reclaimable' as const, limit: 200 }

    await api('top.list', params)
    await api('top.list', params)
    expect(calls).toEqual(['top.list'])
  })

  it('does not cache the live jobs list', async () => {
    const { calls, backend } = countingBackend()
    setBackend(backend)

    await api('jobs.list', {})
    await api('jobs.list', {})
    expect(calls).toEqual(['jobs.list', 'jobs.list'])
  })

  it('forgets answers when a scan or cleanup changes the reports', async () => {
    const { calls, backend } = countingBackend()
    setBackend(backend)
    const params = { target: '/volume1', kind: 'all' as const }

    await api('top.list', params)
    invalidateAnswers()
    await api('top.list', params)
    expect(calls).toEqual(['top.list', 'top.list'])
  })

  it('forgets answers when another backend replaces the recorded one', async () => {
    const first = countingBackend()
    setBackend(first.backend)
    await api('top.list', { target: '/volume1', kind: 'all' })

    const second = countingBackend()
    setBackend(second.backend)
    await api('top.list', { target: '/volume1', kind: 'all' })
    expect(second.calls).toEqual(['top.list'])
  })

  it('passes the abort signal on to the backend', async () => {
    const seen: Array<AbortSignal | undefined> = []
    setBackend(async (_method, _params, options) => {
      seen.push(options?.signal)
      return {} as never
    })
    const controller = new AbortController()

    await api('top.list', { target: '/volume1', kind: 'all' }, { signal: controller.signal })
    expect(seen).toEqual([controller.signal])
  })
})

describe('a root call refused for missing authorization', () => {
  it('tells the app to show the authorization screen and still rejects', async () => {
    const seen: ApiError[] = []
    const stop = onRootCallDenied(error => seen.push(error))
    setBackend(async () => {
      throw new ApiError('authorization_required')
    })

    await expect(api('cleanup.apply', { plan_id: 'plan-1', confirm_move: true })).rejects.toMatchObject({ code: 'authorization_required' })
    expect(seen.map(error => error.code)).toEqual(['authorization_required'])
    stop()
  })

  it('stays quiet for every other failure', async () => {
    const seen: ApiError[] = []
    const stop = onRootCallDenied(error => seen.push(error))
    setBackend(async () => {
      throw new ApiError('busy')
    })

    await expect(api('jobs.list', {})).rejects.toMatchObject({ code: 'busy' })
    expect(seen).toEqual([])
    stop()
  })

  // A raw DSM rejection page surfaces as backend_unavailable; that is not a
  // refused grant and must not open the authorization screen.
  it('does not treat a transport failure as a refused grant', async () => {
    const seen: ApiError[] = []
    const stop = onRootCallDenied(error => seen.push(error))
    setBackend(async () => {
      throw new ApiError('backend_unavailable')
    })

    await expect(api('jobs.list', {})).rejects.toMatchObject({ code: 'backend_unavailable' })
    expect(seen).toEqual([])
    stop()
  })
})
