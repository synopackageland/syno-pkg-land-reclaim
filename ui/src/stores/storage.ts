// sessionStorage and localStorage that never throw (they can be blocked or full).
function read<T>(storage: () => Storage, key: string, fallback: T): T {
  try {
    const raw = storage().getItem(key)
    return raw === null ? fallback : (JSON.parse(raw) as T)
  } catch {
    return fallback
  }
}

function write(storage: () => Storage, key: string, value: unknown) {
  try {
    storage().setItem(key, JSON.stringify(value))
  } catch {
    // Not persisted; the in-memory value still works.
  }
}

export const readSession = <T>(key: string, fallback: T): T => read(() => sessionStorage, key, fallback)
export const writeSession = (key: string, value: unknown) => write(() => sessionStorage, key, value)
/** Kept in this browser across sessions, like the usage alerts' snoozes. */
export const readLocal = <T>(key: string, fallback: T): T => read(() => localStorage, key, fallback)
export const writeLocal = (key: string, value: unknown) => write(() => localStorage, key, value)
