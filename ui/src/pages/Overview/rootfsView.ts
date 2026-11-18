// What the System partition view shows, computed from `rootfs.report`.
// No formatting here: the view formats with t().
import type { RootfsReportDocument } from '@/types'

type RootfsHolder = RootfsReportDocument['open_deleted']['holders'][number]
import { systemPartitionLevel, UPDATE_ROOM_BYTES, type SystemPartitionLevel } from './systemPartition'

type DeepCleanable = NonNullable<RootfsReportDocument['cleanable']['deep']>

/** The categories `rootfs plan` selects, i.e. rows the tool can clean up. */
export const MOVABLE_CATEGORIES = ['update_leftover', 'log_archive', 'crash_dump', 'temp_debris'] as const

/** §2's small-partition note: below this, cleaning may not be enough. */
export const SMALL_PARTITION_BYTES = 3_000_000_000

/** What a row of "What uses the space" leads to: Package Center, the cleanup, or nothing. */
export type OwnerRowKind = 'package' | 'cleanable' | 'dsm' | 'other'

/** DSM's own files: nothing to do but leave them, so they fold into one row. */
const DSM_CATEGORIES = ['dsm_system', 'dsm_keep', 'log_center_db', 'logs', 'geoip_database'] as const

/** One read-only line of a folded row. `small` is the owners under 1 MiB. */
export type OwnerPart = {
  id: string
  category: string
  name: string | null
  bytes: number
  /** Set on the `small` part only. */
  below?: number
  items?: number
}

/** One row of "What uses the space" (§4 S3): a package, a category cleanup moves, or DSM or Other with its parts. */
export type OwnerRow = {
  id: string
  kind: OwnerRowKind
  /** The package or cleanup category; null for DSM and Other. */
  category: string | null
  /** A package's display name, else its id. */
  name: string | null
  bytes: number
  percent: number
  /** DSM's and Other's parts, largest first; empty for the other rows. */
  parts: OwnerPart[]
}

export function ownerRows(report: RootfsReportDocument): OwnerRow[] {
  const used = report.filesystem.used_bytes
  const percent = (bytes: number) => (used > 0 ? (bytes / used) * 100 : 0)
  const rows: OwnerRow[] = []
  const dsm: OwnerPart[] = []
  const other: OwnerPart[] = []
  for (const owner of report.owners) {
    const id = `owner:${owner.category}:${owner.name ?? ''}`
    const part = { id, category: owner.category, name: owner.name, bytes: owner.bytes }
    if (owner.category === 'package_data') {
      const name = owner.display_name?.trim() || owner.name?.trim()
      if (name) rows.push({ id, kind: 'package', category: owner.category, name, bytes: owner.bytes, percent: owner.percent_of_used, parts: [] })
      else dsm.push(part)
    } else if ((MOVABLE_CATEGORIES as readonly string[]).includes(owner.category)) {
      rows.push({ id, kind: 'cleanable', category: owner.category, name: null, bytes: owner.bytes, percent: owner.percent_of_used, parts: [] })
    } else if ((DSM_CATEGORIES as readonly string[]).includes(owner.category)) {
      dsm.push(part)
    } else {
      other.push(part)
    }
  }
  const small = report.owners_small
  if (small) other.push({ id: 'small', category: 'small', name: null, bytes: small.bytes, below: small.below_bytes, items: small.items })
  for (const [kind, parts] of [['dsm', dsm], ['other', other]] as const) {
    if (!parts.length) continue
    const bytes = parts.reduce((sum, part) => sum + part.bytes, 0)
    rows.push({ id: kind, kind, category: null, name: null, bytes, percent: percent(bytes), parts: parts.sort((a, b) => b.bytes - a.bytes) })
  }
  return rows.sort((a, b) => b.bytes - a.bytes)
}

/** The state word, from the report's own filesystem with §2's rules. */
export function reportLevel(report: RootfsReportDocument): SystemPartitionLevel | null {
  return systemPartitionLevel(report.filesystem)
}

/**
 * The small-partition note (§2): the partition is small and moving everything
 * it can would still leave it getting full or full — so cleaning helps little.
 */
export function smallPartitionNote(report: RootfsReportDocument): boolean {
  const { filesystem } = report
  if (filesystem.total_bytes >= SMALL_PARTITION_BYTES) return false
  const afterMoving = { ...filesystem, available_bytes: filesystem.available_bytes + movableBytes(report) }
  const level = systemPartitionLevel(afterMoving)
  return level !== null && level !== 'ok'
}

/** What a deep cleanup would move; null for a report from an older CLI or when it moves nothing. */
export function deepCleanable(report: Pick<RootfsReportDocument, 'cleanable'>): DeepCleanable | null {
  const deep = report.cleanable.deep
  return deep && deep.items > 0 ? deep : null
}

/** The most a cleanup can move off the partition: deep mode's total when the report has one. */
export function movableBytes(report: Pick<RootfsReportDocument, 'cleanable'>): number {
  return Math.max(report.cleanable.bytes, report.cleanable.deep?.bytes ?? 0)
}

/** What is left for the user when even the largest cleanup leaves too little room for a DSM update. */
export type StillShort = {
  /** Available bytes after moving everything the cleanup can. */
  after: number
  /** Software the user installed in /usr/local. */
  userSoftware: number
  /** /root beyond the package caches deep mode moves. */
  rootLeft: number
  /** Deleted files still held open, which a restart frees. */
  heldOpen: number
}

export function stillShort(report: RootfsReportDocument): StillShort | null {
  const after = report.filesystem.available_bytes + movableBytes(report)
  if (after >= UPDATE_ROOM_BYTES) return null
  const owned = (name: string) => report.owners.find(o => o.category === 'user_tooling' && o.name === name)?.bytes ?? 0
  const caches = report.cleanable.deep?.by_category.find(c => c.category === 'tool_cache')?.bytes ?? 0
  return { after, userSoftware: owned('/usr/local'), rootLeft: Math.max(0, owned('/root') - caches), heldOpen: report.open_deleted.bytes_estimate }
}

/** The process holding the most deleted bytes open, for the Details line (§4 S3). */
export function topHolder(holders: RootfsHolder[]): RootfsHolder | null {
  return holders.reduce<RootfsHolder | null>((top, holder) => (top && top.bytes_estimate >= holder.bytes_estimate ? top : holder), null)
}

/** The owner the "nothing movable" card names: the largest of the report's rows. */
export function topOwner(report: RootfsReportDocument): OwnerRow | null {
  return ownerRows(report).reduce<OwnerRow | null>((top, row) => (top && top.bytes >= row.bytes ? top : row), null)
}
