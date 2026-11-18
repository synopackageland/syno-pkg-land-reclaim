// The package CGI transport: one
// same-origin POST of the SDK request body `{method, params}` with the DSM
// session cookie and the SynoToken header. The SDK server on the other side
// checks the session and the admin-only entry gate, and answers the SDK
// envelope. Authorization and business requests use this same transport.
import { createClient } from '@synopackageland/sdk'
import { ApiError, apiErrorFromCgi, apiErrorFromDocument } from './errors'
import type { Backend, Method } from './methods'
import { scheduledBackend, type RequestSender } from './scheduling'

export type CgiRequestInit = {
  method: 'POST'
  body: string
  headers: Record<string, string>
  credentials: 'same-origin'
  signal?: AbortSignal
}

export type CgiResponse = { ok: boolean; status: number; text: () => Promise<string> }
export type CgiFetch = (input: string, init: CgiRequestInit) => Promise<CgiResponse>

/**
 * The DSM session headers for one dispatch (`X-SYNO-TOKEN`). Read at
 * dispatch, never at enqueue, because DSM can renew the token; a thrown error
 * propagates unchanged.
 */
export type CgiHeaderProvider = () => Record<string, string> | Promise<Record<string, string>>

/** Optional host-provided SDK budget in ms; otherwise use the SDK default. */
export type CgiConfig = { url: string; headerProvider?: CgiHeaderProvider; requestTimeout?: number }

export type CgiRequest = { method: string; params?: object }

export type CgiTransport = (request: CgiRequest, options?: { signal?: AbortSignal }) => Promise<unknown>

export function createCgiTransport(fetch: CgiFetch, config: CgiConfig): CgiTransport {
  return async function cgiRequest(request: CgiRequest, options?: { signal?: AbortSignal }): Promise<unknown> {
    const signal = options?.signal
    signal?.throwIfAborted()
    // Fresh per dispatch, because DSM may renew the token while queued.
    const session = config.headerProvider ? await config.headerProvider() : {}
    const client = createClient({
      endpoint: config.url,
      requestTimeout: config.requestTimeout,
      // Adapt the package's small fetch interface. The SDK owns the entire
      // request/body budget and cancellation, with no App timer or override.
      fetch: async (_input, init) => {
        const response = await fetch(config.url, {
          ...init, method: 'POST', body: init?.body as string,
          headers: { ...session, 'Content-Type': 'application/json' },
          credentials: 'same-origin', signal: init?.signal ?? undefined,
        })
        if (!response.ok) throw new ApiError('backend_unavailable')
        return { json: () => parse(response) } as Response
      },
    })
    try {
      return await client.call(request.method, (request.params ?? {}) as Record<string, unknown>, { signal })
    } catch (error) {
      if (error instanceof ApiError) throw error
      const code = (error as { code?: unknown } | null)?.code
      if (code === 'request_canceled' && signal?.aborted) throw signal.reason
      if (code === 'bad_response') throw new ApiError('unknown')
      if (typeof code === 'string') throw apiErrorFromCgi(code, (error as { message?: unknown }).message)
      throw new ApiError('backend_unavailable')
    }
  }
}

/** The package window's ctx.call (SDK Context.call). */
export type WindowCall = (method: string, params?: Record<string, unknown>, options?: { signal?: AbortSignal }) => Promise<unknown>

/** The window's own failures: no answer from the NAS, or no SDK envelope. */
const UNREACHABLE = new Set(['network', 'bad_response'])

/**
 * The transport over the package window's ctx.call: the same request to the
 * same CGI, with the SDK's SynoToken, request deadline and retry of a stopped
 * or starting App. A refusal is mapped as the CGI transport maps it.
 */
export function createCallTransport(call: WindowCall): CgiTransport {
  return async function windowRequest(request: CgiRequest, options?: { signal?: AbortSignal }): Promise<unknown> {
    const signal = options?.signal
    try {
      return await call(request.method, (request.params ?? {}) as Record<string, unknown>, { signal })
    } catch (error) {
      const code = (error as { code?: unknown } | null)?.code
      if (typeof code !== 'string') throw new ApiError('unknown')
      if (code === 'request_canceled' && signal?.aborted) throw signal.reason
      if (UNREACHABLE.has(code)) throw new ApiError('backend_unavailable')
      throw apiErrorFromCgi(code, (error as { message?: unknown }).message)
    }
  }
}

type CgiEnvelope = { success?: boolean; data?: unknown; error?: { code?: unknown; message?: unknown } }

/**
 * The business backend: two-slot scheduling and abort/drop
 * (./scheduling) over the CGI transport. A CLI refusal arrives as its
 * document and is raised here. The DSM mount installs this backend before
 * rendering.
 */
export function cgiBackend(transport: CgiTransport): Backend {
  const send: RequestSender = async (method: Method, params, signal) => {
    const document = await transport({ method, params: params as object }, { signal })
    const refusal = apiErrorFromDocument(document)
    if (refusal) throw refusal
    return document
  }
  return scheduledBackend(send)
}

async function parse(response: CgiResponse): Promise<CgiEnvelope> {
  try {
    const value: unknown = JSON.parse(await response.text())
    return value && typeof value === 'object' ? (value as CgiEnvelope) : {}
  } catch {
    return {}
  }
}
