// Range helpers of the snapshot list: shift-click and "older than a day".

type Timed = { subvolume_id: number; otime: string | null; pending_deletion: boolean }

/**
 * The selection after the grid reports `next`. With an anchor (shift held) and
 * one box changed, every selectable row between the anchor and that box takes
 * its new state. The changed box becomes the next anchor.
 */
export function rangeSelection(
  current: string[],
  next: string[],
  order: string[],
  anchor: string | null,
  selectable: (key: string) => boolean,
): { keys: string[]; anchor: string | null } {
  const before = new Set(current)
  const after = new Set(next)
  const changed = [...order.filter(key => before.has(key) !== after.has(key))]
  if (changed.length !== 1) return { keys: next, anchor: null }
  const key = changed[0]!
  const from = anchor ? order.indexOf(anchor) : -1
  const to = order.indexOf(key)
  if (from < 0 || from === to) return { keys: next, anchor: key }
  const on = after.has(key)
  const keys = new Set(next)
  for (const id of order.slice(Math.min(from, to), Math.max(from, to) + 1)) {
    if (!selectable(id)) continue
    if (on) keys.add(id)
    else keys.delete(id)
  }
  return { keys: order.filter(id => keys.has(id)), anchor: key }
}

function dayStart(otime: string) {
  const date = new Date(otime)
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime()
}

/** Local day starts that have an older snapshot before them, newest first. */
export function daysBefore(snapshots: Timed[]): number[] {
  const times = snapshots.flatMap(snapshot => (snapshot.otime ? [Date.parse(snapshot.otime)] : []))
  if (!times.length) return []
  const oldest = Math.min(...times)
  const days = new Set(snapshots.flatMap(snapshot => (snapshot.otime ? [dayStart(snapshot.otime)] : [])))
  return [...days].filter(day => day > oldest).sort((a, b) => b - a)
}

/** Every snapshot taken before `day`, except those already being deleted. */
export function olderThan(snapshots: Timed[], day: number): string[] {
  return snapshots
    .filter(snapshot => !snapshot.pending_deletion && snapshot.otime && Date.parse(snapshot.otime) < day)
    .map(snapshot => String(snapshot.subvolume_id))
}
