// Layout rules every list page shares (Explore, Duplicates): column widths
// that keep their headers whole, the room a grid's columns fill, and when the
// cleanup bar folds to one row.

/** A page shorter or narrower than this keeps the cleanup bar to one row. */
export const COMPACT_BASKET_BELOW = { width: 900, height: 760 } as const

/**
 * A grid header's space besides its label: 8 px padding on each side, the
 * 1 px border, and the 12 px sort arrow with its 4 px gap.
 */
export const HEADER_CHROME = 33

/** A fixed column's width: `base`, or wider so its header shows in full. */
export function headerWidth(base: number, label: string, measure?: (text: string) => number): number {
  return measure ? Math.max(base, Math.ceil(measure(label)) + HEADER_CHROME) : base
}

/** The 1 px of cell border a collapsed table draws past its columns. */
export const TABLE_EDGE = 1

/** The width a grid's columns fill without scrolling sideways: its host, less the vertical scrollbar and the table's edge. */
export function gridRoom(hostWidth: number, scrollbar: number): number {
  return hostWidth - scrollbar - TABLE_EDGE
}
