// The CGI backend's scheduling and no-replay rules.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from './errors'
import { cgiBackend, createCgiTransport, type CgiFetch, type CgiRequest, type CgiTransport } from './cgi'
import type { Backend } from './methods'

type Harness = {
  backend: Backend
  /** The method of every call the transport has started, in order. */
  names: () => string[]
  succeed: (index: number, value?: unknown) => void
  failSlot: (index: number) => void
  failGrant: (index: number) => void
}

function cgiHarness(): Harness {
  const calls: { name: string; resolve: (value: unknown) => void; reject: (error: unknown) => void }[] = []
  const transport: CgiTransport = request =>
    new Promise((resolve, reject) => {
      calls.push({ name: request.method, resolve, reject })
    })
  const backend = cgiBackend(transport)
  return {
    backend,
    names: () => calls.map(call => call.name),
    succeed: (index, value = {}) => calls[index].resolve(value),
    failSlot: index => calls[index].reject(new ApiError('backend_busy')),
    failGrant: index => calls[index].reject(new ApiError('authorization_required', undefined, { reason: 'grant_missing' })),
  }
}

afterEach(() => vi.useRealTimers())

describe('business scheduling', () => {
  it('runs one normal query at a time with the poll in the reserved slot', () => {
    const h = cgiHarness()
    void h.backend('top.list', { target: '/volume1', kind: 'all' })
    void h.backend('overview.get', { volume: '/volume1' })
    void h.backend('jobs.list', {})
    expect(h.names()).toEqual(['top.list', 'jobs.list'])
  })

  it('runs a stop action before queued normal queries', () => {
    const h = cgiHarness()
    for (const method of ['top.list', 'overview.get', 'volumes.list', 'history.list'] as const) void h.backend(method, {})
    void h.backend('job.cancel', { job_id: 'x' })
    expect(h.names()).toEqual(['top.list', 'job.cancel'])
  })

  it('drops a superseded queued query before it takes a slot', async () => {
    const h = cgiHarness()
    const controller = new AbortController()
    void h.backend('top.list', { target: '/volume1', kind: 'all' })
    const rejection = expect(h.backend('overview.get', { volume: '/volume1' }, { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' })
    void h.backend('volumes.list', {})
    expect(h.names()).toHaveLength(1)

    controller.abort()
    h.succeed(0)
    await vi.waitFor(() => expect(h.names()).toHaveLength(2))
    expect(h.names()[1]).toBe('volumes.list')
    h.succeed(1)
    await rejection
  })
})

describe('CGI backend request shape', () => {
  it('sends a business method as the SDK request', async () => {
    const requests: CgiRequest[] = []
    const backend = cgiBackend(request => {
      requests.push(request)
      return Promise.resolve({ volumes: [] })
    })

    await backend('volumes.list', {})
    const params = { targets: ['/volume1'], mode: 'gentle' as const }
    await backend('scan.start', params)

    expect(requests).toEqual([
      { method: 'volumes.list', params: {} },
      { method: 'scan.start', params },
    ])
  })

  it('raises a CLI refusal the document carries', async () => {
    const backend = cgiBackend(() => Promise.resolve({ completion: 'busy', outcome: 'scan_in_progress', running_job_id: 's1', exit: 4 }))
    await expect(backend('scan.start', { targets: ['/volume1'], mode: 'gentle' })).rejects.toMatchObject({
      code: 'scan_in_progress',
      detail: { running_job_id: 's1' },
    })
  })
})

describe('CGI session header dispatch', () => {
  it('reads the token again for each request', async () => {
    const tokens: string[] = []
    const fetch: CgiFetch = async (_input, init) => {
      tokens.push(init.headers['X-SYNO-TOKEN'] ?? '')
      return { ok: true, status: 200, text: async () => JSON.stringify({ success: true, data: { volumes: [] } }) }
    }
    let issued = 0
    const backend = cgiBackend(createCgiTransport(fetch, { url: 'cgi', headerProvider: () => ({ 'X-SYNO-TOKEN': `token-${(issued += 1)}` }) }))

    await backend('volumes.list', {})
    await backend('volumes.list', {})
    expect(tokens).toEqual(['token-1', 'token-2'])
  })

  it('reads no token for a call still waiting for a slot', async () => {
    const resolvers: (() => void)[] = []
    const fetch: CgiFetch = () => new Promise(resolve => {
      resolvers.push(() => resolve({ ok: true, status: 200, text: async () => JSON.stringify({ success: true, data: {} }) }))
    })
    let issued = 0
    const backend = cgiBackend(createCgiTransport(fetch, { url: 'cgi', headerProvider: () => { issued += 1; return { 'X-SYNO-TOKEN': `token-${issued}` } } }))

    void backend('top.list', { target: '/volume1', kind: 'all' })
    void backend('overview.get', { volume: '/volume1' })
    expect(issued).toBe(1)

    await vi.waitFor(() => expect(resolvers).toHaveLength(1))
    resolvers[0]()
    await vi.waitFor(() => expect(issued).toBe(2))
  })

  it('gives two overlapping dispatches their own token read', async () => {
    let issued = 0
    const tokens: string[] = []
    const resolvers: (() => void)[] = []
    const fetch: CgiFetch = (_input, init) => {
      tokens.push(init.headers['X-SYNO-TOKEN'] ?? '')
      return new Promise(resolve => {
        resolvers.push(() => resolve({ ok: true, status: 200, text: async () => JSON.stringify({ success: true, data: {} }) }))
      })
    }
    const backend = cgiBackend(createCgiTransport(fetch, { url: 'cgi', headerProvider: () => ({ 'X-SYNO-TOKEN': `token-${(issued += 1)}` }) }))

    const both = Promise.all([
      backend('top.list', { target: '/volume1', kind: 'all' }),
      backend('jobs.list', {}),
    ])
    await vi.waitFor(() => expect(tokens).toHaveLength(2))
    expect(tokens).toEqual(['token-1', 'token-2'])
    for (const resolve of resolvers) resolve()
    await both
  })
})

describe('CGI backend no-replay rules', () => {
  it('surfaces a busy refusal without adding an App retry', async () => {
    vi.useFakeTimers()
    const h = cgiHarness()
    const pending = h.backend('volumes.list', {})
    const rejection = expect(pending).rejects.toMatchObject({ code: 'backend_busy' })
    h.failSlot(0)
    await vi.advanceTimersByTimeAsync(5000)
    await rejection
    expect(h.names()).toHaveLength(1)
  })

  it.each(['cleanup.apply', 'rootfs.apply'] as const)('never automatically resends %s after any failure', async method => {
    vi.useFakeTimers()
    const h = cgiHarness()
    const params = method === 'cleanup.apply'
      ? { plan_id: 'p1', confirm_move: true as const }
      : { plan_id: 'p1', confirm_no_restore: true as const }
    const pending = h.backend(method, params)
    const rejection = expect(pending).rejects.toMatchObject({ code: 'backend_busy' })
    h.failSlot(0)
    await vi.advanceTimersByTimeAsync(5000)
    await rejection
    expect(h.names()).toHaveLength(1)
  })

  it('stops on a refused authorization instead of resending the operation', async () => {
    const h = cgiHarness()
    const pending = h.backend('cleanup.apply', { plan_id: 'p1', confirm_move: true })
    const rejection = expect(pending).rejects.toMatchObject({ code: 'authorization_required' })
    h.failGrant(0)
    await rejection
    expect(h.names()).toHaveLength(1)
  })
})
