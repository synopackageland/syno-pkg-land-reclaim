// The shared two-slot scheduler behind every business backend.
// The transport is injected: the signed DSM shim and the unsigned CGI
// keep identical scheduling and abort/drop behavior; nothing here
// knows the request shape.
import type { Backend, Method } from './methods'

/** One raw call of a transport (DSM `SYNO.API.Request` or the package CGI). */
export type RequestSender = (method: Method, params: object, signal?: AbortSignal) => Promise<unknown>

// The transport runs at most two operations, so never more than two calls are
// in flight. Normal page queries get one slot; the other stays for job polling
// and stop actions, which may also use the free normal slot.
const MAX_IN_FLIGHT = 2
const NORMAL_IN_FLIGHT = 1

/** Job polling and stop actions must not queue behind background queries (design review §1). */
const PRIORITY_METHODS = new Set<Method>(['jobs.list', 'job.pause', 'job.resume', 'job.cancel', 'job.result'])

export function isPriorityMethod(method: Method) {
  return PRIORITY_METHODS.has(method)
}

/** One queued call: start it when a slot frees, or drop it when its signal aborted. */
type Pending = { start: () => void; drop: () => void; signal?: AbortSignal }

function abortReason(signal: AbortSignal) {
  return signal.reason ?? new DOMException('superseded', 'AbortError')
}

export function scheduledBackend(send: RequestSender): Backend {
  const priority: Pending[] = []
  const normal: Pending[] = []
  let inFlight = 0
  let normalInFlight = 0

  function run(next: () => void, isNormal: boolean) {
    inFlight += 1
    if (isNormal) normalInFlight += 1
    next()
  }

  // A query whose navigation was superseded is dropped here, before it takes
  // one of the two slots (design review 2026-10-01 §7.3). Priority methods
  // carry no signal and are never dropped or delayed.
  function take(queue: Pending[]): Pending | null {
    while (queue.length > 0) {
      const next = queue.shift()!
      if (next.signal?.aborted) {
        next.drop()
        continue
      }
      return next
    }
    return null
  }

  function dispatch() {
    let next: Pending | null
    // Priority first: polling and stop actions take the reserved slot and may
    // also use the free normal slot, never exceeding the two the transport has.
    while (inFlight < MAX_IN_FLIGHT && (next = take(priority))) {
      run(next.start, false)
    }
    while (inFlight < MAX_IN_FLIGHT && normalInFlight < NORMAL_IN_FLIGHT && (next = take(normal))) {
      run(next.start, true)
    }
  }

  return (method, params, options) =>
    new Promise((resolve, reject) => {
      const isNormal = !isPriorityMethod(method)
      const signal = isNormal ? options?.signal : undefined
      if (signal?.aborted) {
        reject(abortReason(signal))
        return
      }
      const start = () => {
        send(method, params, signal)
          .then(value => resolve(value as never), reject)
          .finally(() => {
            inFlight -= 1
            if (isNormal) normalInFlight -= 1
            dispatch()
          })
      }
      // `drop` is only called for an aborted signal, so the reason is set.
      ;(isNormal ? normal : priority).push({ start, signal, drop: () => reject(abortReason(signal!)) })
      dispatch()
    })
}
