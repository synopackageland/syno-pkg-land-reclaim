import { clientTimeout, DEFAULT_REQUEST_TIMEOUT } from '@synopackageland/sdk'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from './errors'
import { createCallTransport, createCgiTransport, type CgiFetch } from './cgi'

/** A fetch double that records the request and answers one body. */
function fetchOf(response: { ok?: boolean; status?: number; body: string }) {
  const calls: { input: string; init: { method: string; body: string; headers: Record<string, string>; credentials: string; signal?: AbortSignal } }[] = []
  const fetch: CgiFetch = async (input, init) => {
    calls.push({ input, init })
    return { ok: response.ok ?? true, status: response.status ?? 200, text: async () => response.body }
  }
  return { calls, fetch }
}

const CONFIG = { url: 'webman/3rdparty/SynoReclaim/cgi/api.cgi' }
const REQUEST = { method: 'authorization.status', params: {} }

describe('CGI transport', () => {
  it('posts the SDK request body with the DSM session cookie', async () => {
    const { calls, fetch } = fetchOf({ body: JSON.stringify({ success: true, data: { state: 'ready' } }) })
    const data = await createCgiTransport(fetch, CONFIG)(REQUEST)

    expect(data).toEqual({ state: 'ready' })
    expect(calls).toHaveLength(1)
    expect(calls[0].input).toBe(CONFIG.url)
    expect(calls[0].init.method).toBe('POST')
    expect(calls[0].init.credentials).toBe('same-origin')
    expect(calls[0].init.headers['Content-Type']).toBe('application/json')
    expect(JSON.parse(calls[0].init.body)).toEqual({ method: 'authorization.status', params: {} })
  })

  it('carries no header of its own besides the JSON content type', async () => {
    const { calls, fetch } = fetchOf({ body: JSON.stringify({ success: true, data: {} }) })
    await createCgiTransport(fetch, CONFIG)(REQUEST)
    expect(Object.keys(calls[0].init.headers)).toEqual(['Content-Type'])
  })

  // DSM can renew the SynoToken while the window is open, so it is read at
  // dispatch, never at enqueue.
  it('reads the session headers on every dispatch', async () => {
    let issued = 0
    const { calls, fetch } = fetchOf({ body: JSON.stringify({ success: true, data: {} }) })
    const transport = createCgiTransport(fetch, { url: CONFIG.url, headerProvider: () => ({ 'X-SYNO-TOKEN': `token-${(issued += 1)}` }) })

    await transport(REQUEST)
    await transport(REQUEST)
    expect(calls.map(call => call.init.headers['X-SYNO-TOKEN'])).toEqual(['token-1', 'token-2'])
  })

  it('keeps its JSON content type and merges the session headers', async () => {
    const { calls, fetch } = fetchOf({ body: JSON.stringify({ success: true, data: {} }) })
    const transport = createCgiTransport(fetch, { url: CONFIG.url, headerProvider: async () => ({ 'X-SYNO-TOKEN': 't', 'Content-Type': 'text/plain' }) })
    await transport(REQUEST)
    expect(calls[0].init.headers).toEqual({ 'X-SYNO-TOKEN': 't', 'Content-Type': 'application/json' })
  })

  it('maps the SDK session code to session_timeout', async () => {
    const { fetch } = fetchOf({ body: JSON.stringify({ success: false, error: { code: 'session_required', message: 'sign in to DSM again' } }) })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ name: 'ApiError', code: 'session_timeout' })
  })

  it('asks for approval when the executor does not answer', async () => {
    const { fetch } = fetchOf({
      body: JSON.stringify({
        success: false,
        error: { code: 'EXECUTOR_UNAVAILABLE', message: 'the executor is not running' },
      }),
    })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({
      code: 'authorization_required',
      detail: { cgi_code: 'EXECUTOR_UNAVAILABLE', reason: 'grant_missing' },
    })
  })

  it('maps an unavailable auth service to auth_unavailable, retryable', async () => {
    const { fetch } = fetchOf({ body: JSON.stringify({ success: false, error: { code: 'auth_unavailable', message: 'DSM authentication is unavailable' } }) })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({
      code: 'auth_unavailable',
      detail: { cgi_code: 'auth_unavailable', reason: 'service_unavailable' },
    })
  })

  it('reports a missing CGI endpoint as backend_unavailable, to retry, not to authorize', async () => {
    const fetch: CgiFetch = async () => ({ ok: false, status: 404, text: async () => 'not found' })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ code: 'backend_unavailable' })
  })

  it('reports a network failure as backend_unavailable', async () => {
    const fetch: CgiFetch = async () => {
      throw new TypeError('Failed to fetch')
    }
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toBeInstanceOf(ApiError)
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ code: 'backend_unavailable' })
  })

  it('treats a body that is not an API envelope as unknown', async () => {
    const { fetch } = fetchOf({ body: '<html>login</html>' })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ code: 'unknown' })
  })

  // DSM can answer an HTTP 403 text/html page before any CGI JSON. Fail closed
  // as a non-grant failure; never ask for authorization again.
  it('fails closed on a DSM HTML 403 that arrives before the CGI JSON', async () => {
    const fetch: CgiFetch = async () => ({ ok: false, status: 403, text: async () => '<!DOCTYPE html><html><body>Access denied</body></html>' })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ code: 'backend_unavailable' })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.not.toMatchObject({ code: 'authorization_required' })
  })

  it('fails closed on a non-JSON 200 page instead of guessing success', async () => {
    const fetch: CgiFetch = async () => ({ ok: true, status: 200, text: async () => '<html>login</html>' })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toBeInstanceOf(ApiError)
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.not.toMatchObject({ code: 'authorization_required' })
  })

  it('stops following the caller signal after the SDK request settles', async () => {
    const controller = new AbortController()
    const { calls, fetch } = fetchOf({ body: JSON.stringify({ success: true, data: {} }) })
    await createCgiTransport(fetch, CONFIG)(REQUEST, { signal: controller.signal })
    expect(calls[0].init.signal?.aborted).toBe(false)
    controller.abort()
    expect(calls[0].init.signal?.aborted).toBe(false)
  })

  it('maps a CLI failure without a document to cli_error', async () => {
    const { fetch } = fetchOf({ body: JSON.stringify({ success: false, error: { code: 'cli_error', message: 'the CLI returned no document' } }) })
    await expect(createCgiTransport(fetch, CONFIG)(REQUEST)).rejects.toMatchObject({ code: 'cli_error' })
  })

  describe('request deadline', () => {
    afterEach(() => vi.useRealTimers())

    /** A fetch that answers only when its signal aborts. */
    const hanging: CgiFetch = (_input, init) =>
      new Promise((_resolve, reject) => init.signal?.addEventListener('abort', () => reject(init.signal!.reason)))

    // An explicit host budget is still owned by the SDK client.
    it('gives up as a timeout once the window\'s request deadline has passed', async () => {
      vi.useFakeTimers()
      let settled: unknown
      createCgiTransport(hanging, { ...CONFIG, requestTimeout: 140_000 })(REQUEST).catch(error => (settled = error))
      await vi.advanceTimersByTimeAsync(clientTimeout(140_000) - 1)
      expect(settled).toBeUndefined()
      await vi.advanceTimersByTimeAsync(1)
      expect(settled).toMatchObject({ name: 'ApiError', code: 'timeout' })
    })

    it('waits for the SDK default deadline without one', async () => {
      vi.useFakeTimers()
      let settled: unknown
      createCgiTransport(hanging, CONFIG)(REQUEST).catch(error => (settled = error))
      await vi.advanceTimersByTimeAsync(clientTimeout(DEFAULT_REQUEST_TIMEOUT) - 1)
      expect(settled).toBeUndefined()
      await vi.advanceTimersByTimeAsync(1)
      expect(settled).toMatchObject({ name: 'ApiError', code: 'timeout' })
    })

    it('keeps the SDK budget active while reading the response body', async () => {
      vi.useFakeTimers()
      let bodySignal: AbortSignal | undefined
      const fetch: CgiFetch = async (_input, init) => {
        bodySignal = init.signal
        return { ok: true, status: 200, text: () => new Promise((_resolve, reject) => {
          init.signal?.addEventListener('abort', () => reject(init.signal!.reason), { once: true })
        }) }
      }
      const pending = createCgiTransport(fetch, CONFIG)(REQUEST).catch(error => error)
      await vi.advanceTimersByTimeAsync(clientTimeout(DEFAULT_REQUEST_TIMEOUT))
      expect(bodySignal?.aborted).toBe(true)
      await expect(pending).resolves.toMatchObject({ code: 'timeout' })
    })

    it('still lets the caller abort a request, which is no timeout', async () => {
      const controller = new AbortController()
      const request = createCgiTransport(hanging, CONFIG)(REQUEST, { signal: controller.signal })
      controller.abort(new DOMException('superseded', 'AbortError'))
      await expect(request).rejects.toMatchObject({ name: 'AbortError' })
    })
  })
})

// The package window's ctx.call posts the same body to the same CGI, with the
// SynoToken, the window's request deadline and the SDK's own retry of a
// stopped or starting App; its refusals are plain {code, message} objects.
describe('the window call transport', () => {
  it('sends the method and its params through the window call and passes the caller signal', async () => {
    const call = vi.fn(async () => ({ state: 'ready' }))
    const signal = new AbortController().signal
    const data = await createCallTransport(call)({ method: 'volumes.list', params: { a: 1 } }, { signal })

    expect(data).toEqual({ state: 'ready' })
    expect(call).toHaveBeenCalledWith('volumes.list', { a: 1 }, { signal })
  })

  it('maps an SDK refusal as the CGI transport does', async () => {
    const call = vi.fn(async () => { throw { code: 'session_required', message: 'x' } })
    await expect(createCallTransport(call)(REQUEST)).rejects.toMatchObject({ code: 'session_timeout' })
  })

  it.each(['network', 'bad_response'])('reports the window call failure %s as backend_unavailable', async code => {
    const call = vi.fn(async () => { throw { code, message: 'x' } })
    await expect(createCallTransport(call)(REQUEST)).rejects.toMatchObject({ code: 'backend_unavailable' })
  })

  it("rejects with the caller's abort reason when the page leaves", async () => {
    const controller = new AbortController()
    const reason = new DOMException('superseded', 'AbortError')
    const call = vi.fn(async () => {
      controller.abort(reason)
      throw { code: 'request_canceled', message: 'x' }
    })
    await expect(createCallTransport(call)(REQUEST, { signal: controller.signal })).rejects.toBe(reason)
  })

  it('reports anything that is not a refusal as unknown', async () => {
    const call = vi.fn(async () => { throw new TypeError('boom') })
    const error: unknown = await createCallTransport(call)(REQUEST).catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect((error as ApiError).code).toBe('unknown')
  })
})


it('keeps the app resume explanation through both authenticated transports', async () => {
  const message = 'Saved subvolume identity or generation changed; start a fresh scan'
  const { fetch } = fetchOf({ body: JSON.stringify({ success: false, error: { code: 'checkpoint_unusable', message } }) })
  const transports = [
    createCgiTransport(fetch, { url: '/cgi' }),
    createCallTransport(async () => { throw { code: 'checkpoint_unusable', message } }),
  ]
  for (const transport of transports) {
    await expect(transport({ method: 'job.resume' })).rejects.toMatchObject({ detail: { cgi_code: 'checkpoint_unusable', resume_message: message } })
  }
})
