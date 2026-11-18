import { ApiError, sdkRefusal } from './errors'
import { cachedAnswer, clearAnswers } from './answerCache'
import type { Backend, Method, ParamsOf, RequestOptions, ResultOf } from './methods'

export { ApiError } from './errors'
export type { ApiErrorCode } from './errors'
export type { Method, ParamsOf, PathParam, RequestOptions, ResultOf, ScanMode } from './methods'

/** A root call the backend refused because the authorization lapsed, or one the SDK names (sdkRefusal). */
type RootCallDenied = (error: ApiError) => void
const deniedListeners = new Set<RootCallDenied>()

/**
 * Watch for a refused root call: the app
 * shows the authorization screen, stops further root work, and never replays
 * the refused cleanup/apply by itself.
 */
export function onRootCallDenied(listener: RootCallDenied) {
  deniedListeners.add(listener)
  return () => {
    deniedListeners.delete(listener)
  }
}

let backend: Promise<Backend> | undefined

async function chooseBackend(): Promise<Backend> {
  if (import.meta.env.DEV) {
    const { fixtureBackend, scenarioFromUrl } = await import('./fixtures')
    return fixtureBackend({ scenario: scenarioFromUrl() })
  }
  return () => Promise.reject(new ApiError('backend_unavailable'))
}

/**
 * Report answers that are a pure function of the saved report and worth
 * caching in the page (design review 2026-10-01 §7.3): drill-downs and the
 * Recycle Bin pages. Live calls (jobs, session, volumes) and calls a page can
 * force again (overview.get, inspect.get) are never cached here.
 */
const CACHEABLE_METHODS = new Set<Method>(['top.list', 'recycle.list'])

/** The only way the UI talks to the backend. */
export async function api<M extends Method>(method: M, params: ParamsOf<M>, options?: RequestOptions): Promise<ResultOf<M>> {
  backend ??= chooseBackend()
  const service = await backend
  // Callers without a request option keep the plain two-argument call.
  const run = () => (options ? service(method, params, options) : service(method, params))
  try {
    if (!CACHEABLE_METHODS.has(method)) return await run()
    return await cachedAnswer(method, params as Record<string, unknown>, run)
  } catch (error) {
    if (error instanceof ApiError && (error.code === 'authorization_required' || sdkRefusal(error))) {
      for (const listener of deniedListeners) listener(error)
    }
    throw error
  }
}

/** Replace the backend, e.g. `setBackend(fixtureBackend({ scenario: 'paused' }))` in a test. */
export function setBackend(next: Backend) {
  backend = Promise.resolve(next)
  // Answers from the replaced backend describe reports it had, not the new one.
  clearAnswers()
}
