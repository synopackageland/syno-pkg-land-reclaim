/**
 * Explore layout. The DSM app window may be narrower than the browser
 * viewport (the window `synopkgland build` generates), so the treemap-above-table decision follows
 * the measured content width, not a viewport media query.
 */

/**
 * How the SplitPane shares Explore: the treemap takes the larger
 * share side by side; the list keeps room for its selection box, a name and
 * the Size and Can free columns, so a 1000 px DSM window stays side by side.
 * Stacked, the map stays low (about 130 px of tiles) and the list keeps its rows.
 */
export const EXPLORE_SPLIT = {
  horizontal: { defaultSize: 0.56, minStart: 260, minEnd: 420 },
  vertical: { defaultSize: 0.3, minStart: 180, minEnd: 240 },
} as const

/** The SplitPane separator's thickness. */
export const EXPLORE_SPLIT_GAP = 12

/** The content width below which the treemap moves above the table: side by side, both minimums must fit. */
export const EXPLORE_STACK_BELOW = 700

/** A map panel narrower than this keeps its colour key behind an icon. */
export const LEGEND_INLINE_MIN = 520

export function exploreStacked(width: number): boolean {
  return width < EXPLORE_STACK_BELOW
}

/** The name's width below which the list folds Held by and Where to manage into each row's second line. */
export const NAME_FULL_MIN = 200
export const WHERE_MIN_WIDTH = 120
export const WHERE_MAX_WIDTH = 240

/**
 * The list's text columns from the room its name, Held by and Where to manage
 * columns share. A narrow list keeps the name alone (`held` and `where` null)
 * and says the rest under it; a wide one gives Where to manage up to 40% of
 * what Held by leaves, never taking the name below `NAME_FULL_MIN`.
 */
export function listColumns(room: number, held: number): { name: number; held: number | null; where: number | null } {
  if (!Number.isFinite(room)) return { name: 300, held, where: WHERE_MIN_WIDTH }
  if (room < NAME_FULL_MIN + held + WHERE_MIN_WIDTH) return { name: room, held: null, where: null }
  const free = room - held
  const where = Math.min(WHERE_MAX_WIDTH, free - NAME_FULL_MIN, Math.max(WHERE_MIN_WIDTH, Math.round(free * 0.4)))
  return { name: free - where, held, where }
}

/**
 * The list's width with Held by and Where to manage as columns: the selection
 * box, Size, Can free, the shortest full name, Held by and Where to manage, a
 * classic scrollbar and the panel's edges, with room for wider headers.
 */
export const LIST_FULL_WIDTH = 700

/** The least of the width the treemap starts with side by side. */
export const MAP_MIN_SHARE = 0.4

/**
 * The share the treemap starts with when the user has not moved the split:
 * side by side, up to 56%, less where the list would otherwise fold its
 * columns into cut-off second lines (a maximized 1440 px window), never below 40%.
 */
export function exploreSplitDefault(orientation: 'horizontal' | 'vertical', width: number): number {
  const base = EXPLORE_SPLIT[orientation].defaultSize
  if (orientation === 'vertical' || !Number.isFinite(width) || width <= 0) return base
  const leavingListFull = 1 - (LIST_FULL_WIDTH + EXPLORE_SPLIT_GAP) / width
  return Math.round(Math.min(base, Math.max(MAP_MIN_SHARE, leavingListFull)) * 1e4) / 1e4
}
