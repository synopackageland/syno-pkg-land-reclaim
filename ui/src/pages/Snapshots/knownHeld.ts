// What a report without per-snapshot data already says about snapshots: the
// rows of the volume-level top.list whose bytes snapshots also hold, as Explore
// draws them ("Held by" snapshot). No formatting here.
import { serviceName } from '@/pages/Explore/treemap'
import type { TopDocument } from '@/types'

type TopResult = TopDocument['results'][number]
type Entry = TopResult['entries'][number]

export type HeldRow = {
  key: string
  /** The package for package data, else the folder or file name. */
  name: string
  path: string
  /** The volume-root folder the row is or lies in; the row's own name when it is one. */
  folder: string
  snapshotBytes: number
}

export type SnapshotHeld = {
  rows: HeldRow[]
  /** The rows' snapshot bytes added up: each byte once, as no row includes another. */
  total: number
  /** The answer reached its row limit, so more rows may hold some. */
  lowerBound: boolean
}

const snapshotBytes = (entry: Entry) =>
  entry.held_by.reduce((sum, held) => sum + (held.class === 'snapshot' ? held.bytes : 0), 0)

const baseName = (path: string) => path.replace(/\/$/, '').split('/').at(-1) || path

/** The first name of `path` below the volume `mount`. */
function folderOf(path: string, mount: string | undefined) {
  const below = mount && path.startsWith(`${mount}/`) ? path.slice(mount.length + 1) : path.split('/').slice(2).join('/')
  return below.split('/')[0] || baseName(path)
}

/** The rows of `result` snapshots also hold, largest first; `limit` is the row limit asked for. */
export function snapshotHeld(result: TopResult | undefined, limit: number): SnapshotHeld {
  if (!result) return { rows: [], total: 0, lowerBound: false }
  // A row included in another (`within`) is already counted there (Explore's treemap rule).
  const rows = result.entries
    .filter(entry => !entry.within && snapshotBytes(entry) > 0)
    .map(entry => ({
      key: entry.path_hex ?? entry.path,
      name: entry.service ? serviceName(entry.service.id) : baseName(entry.path),
      path: entry.path,
      folder: folderOf(entry.path, result.target?.path),
      snapshotBytes: snapshotBytes(entry),
    }))
    .sort((a, b) => b.snapshotBytes - a.snapshotBytes)
  return {
    rows,
    total: rows.reduce((sum, row) => sum + row.snapshotBytes, 0),
    lowerBound: result.entries.length >= limit,
  }
}
