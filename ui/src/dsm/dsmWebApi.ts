import type { Context } from '@synopackageland/sdk'

/** DSM owns the session and transport; missing window access fails closed. */
export async function dsmWebApi(ctx: Pick<Context, 'webApi'>, api: string, method: string, version: number, params: Record<string, unknown>, signal?: AbortSignal): Promise<unknown> {
  signal?.throwIfAborted()
  if (!ctx.webApi) throw { code: 'dsm_unavailable' }
  let abort: (() => void) | undefined
  const cancelled = new Promise<never>((_, reject) => {
    abort = () => reject(signal?.reason)
    signal?.addEventListener('abort', abort, { once: true })
  })
  try {
    // SDK refusals (dsm_<code>) reach the inventory's existing null/unknown
    // mapping. Cancellation bounds waiting; the SDK owns the in-flight read.
    return await Promise.race([ctx.webApi(api, method, version, params), cancelled])
  } finally {
    if (abort) signal?.removeEventListener('abort', abort)
  }
}
