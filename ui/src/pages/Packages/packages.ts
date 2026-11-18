// What the Packages page shows: the service lines of a
// volume-level top.list, one row per line. No formatting here.
import type { Path, TopDocument } from '@/types'

type TopResult = TopDocument['results'][number]
type Line = TopResult['services']['lines'][number]
type ServiceEntry = TopResult['entries'][number]

/** The most paths a service row lists; the CLI's own limit (SPEC 4.6). */
export const LISTED_PATHS = 20

export type PackageRow = {
  /** Service code plus path: unique among the rows. */
  key: string
  service: string
  /** What the CLI calls the package, in English; the page localizes by `service`. */
  package: string
  names: string
  /** Present when the line is one path. */
  path?: string
  paths: Path[]
  pathCount: number
  physical: number
  /** Only freed through the package; null when unknown. */
  canFree: number | null
  /** Null when the answer holds no entry of the line to read it from. */
  heldBySnapshots: number | null
  unknown: number
  snapshotsNotRead: number
  manageInId: string
  confirmed: boolean
  /** The shared folder the line's data lies in; null for system areas at the volume root. */
  sharedFolder: string | null
}

const sameLine = (line: Line) => (entry: ServiceEntry) =>
  entry.service?.id === line.service && (line.path === undefined || entry.path === line.path)

const snapshotBytes = (entry: ServiceEntry) =>
  entry.held_by.reduce((sum, held) => sum + (held.class === 'snapshot' ? held.bytes : 0), 0)

function linePaths(line: Line, entries: ServiceEntry[]): Path[] {
  const paths = entries.flatMap(entry => entry.service?.paths ?? [])
  if (paths.length) {
    const unique = new Map(paths.map(path => [path.path_hex ?? path.path, path]))
    return [...unique.values()].slice(0, LISTED_PATHS)
  }
  return line.path === undefined ? [] : [{ path: line.path, ...(line.path_hex ? { path_hex: line.path_hex } : {}) }]
}

/** The name of the shared folder `path` is or lies in; null outside every share. */
function sharedFolderOf(path: string | undefined, shares: readonly string[]): string | null {
  if (!path) return null
  const share = shares.find(share => path === share || path.startsWith(`${share}/`))
  return share ? share.split('/').at(-1) ?? null : null
}

function toRow(line: Line, entries: ServiceEntry[], shares: readonly string[]): PackageRow {
  const matching = entries.filter(sameLine(line))
  const complete = matching.reduce((sum, entry) => sum + (entry.service?.path_count ?? 0), 0) === line.paths
  const paths = linePaths(line, matching)
  return {
    key: `${line.service}|${line.path ?? ''}`,
    service: line.service,
    package: line.package,
    names: line.names,
    path: line.path,
    paths,
    pathCount: line.paths,
    physical: line.metrics.physical,
    canFree: line.metrics.reclaimable,
    heldBySnapshots: complete ? matching.reduce((sum, entry) => sum + snapshotBytes(entry), 0) : null,
    unknown: line.metrics.unknown,
    snapshotsNotRead: line.snapshots_not_read,
    manageInId: line.manage_in_id,
    confirmed: line.confirmed,
    sharedFolder: sharedFolderOf(line.path ?? paths[0]?.path, shares),
  }
}

/** Every line Explore shows as package-managed data is a row here, in a shared
 * folder or not. Recycle Bins have their own page; folder
 * metadata is separate information. */
const OUTSIDE_RANKING = ['recycle_bin', 'folder_metadata']

/** Whether a service's data is listed as package space. */
export function inPackageArea(service: string) {
  return !OUTSIDE_RANKING.includes(service)
}

/** Where a package row's space is handled: here, in its package, or not at all. */
export type Handling = 'here' | 'in_package' | 'keep'

/** Data the system or a package needs to run; removing the package is the only way to free it. */
const KEPT = ['packages', 'package_data', 'package_home', 'package_config', 'package_share', 'package_temp', 'databases', 'dsm_temp', 'dsm_quota', 'other_dsm_service_data']

/** Services with their own "how to free it" text (`packages.how.<key>`). */
const HOW = [
  'synology_drive', 'synology_drive_sharesync', 'active_backup', 'active_backup_microsoft_365', 'active_backup_google_workspace',
  'container_manager', 'share_snapshots', 'snapshot_replication', 'surveillance_data', 'iscsi_luns', 'vmm_virtual_disks',
  'cloud_sync', 'usb_copy', 'hyper_backup_cache', 'syno_reclaim', 'packages', 'databases', 'dsm_temp', 'dsm_quota', 'other_dsm_service_data',
]
const SAME_HOW: Record<string, string> = {
  active_backup_volume: 'active_backup',
  package_data: 'package_files',
  package_home: 'package_files',
  package_config: 'package_files',
  package_share: 'package_files',
  package_temp: 'package_files',
}

/** The `packages.how.<key>` that says how to free a service's space; `default` names its place. */
export function howKey(service: string): string {
  return SAME_HOW[service] ?? (HOW.includes(service) ? service : 'default')
}

export function handling(service: string): Handling {
  if (service === 'syno_reclaim') return 'here'
  return KEPT.includes(service) ? 'keep' : 'in_package'
}

/** The rows of one top.list result, largest physical size first; `shares` are the volume's share paths.
 * An empty line of a package that already has a row is left out: one row per
 * package says it is there (a test NAS: three empty VDISK_* lines). */
export function packageRows(result: TopResult | undefined, shares: readonly string[] = []): PackageRow[] {
  if (!result) return []
  const listed = new Set<string>()
  return result.services.lines
    .filter(line => inPackageArea(line.service))
    .map(line => toRow(line, result.entries, shares))
    .sort((a, b) => b.physical - a.physical)
    .filter(row => {
      const first = !listed.has(row.service)
      listed.add(row.service)
      return first || row.physical > 0
    })
}

/** The row a clicked service entry stands for: its own path, else its service's first row. */
export function findRow(rows: PackageRow[], service: string, path?: string): PackageRow | undefined {
  const ofService = rows.filter(row => row.service === service)
  return ofService.find(row => row.path === path) ?? ofService.find(row => row.path === undefined) ?? ofService[0]
}

/** Folder metadata is information, not another package to add to folder sizes. */
export function folderMetadata(result: TopResult | undefined): PackageRow | undefined {
  if (!result) return undefined
  const line = result.services.lines.find(line => line.service === 'folder_metadata')
  return line && toRow(line, result.entries, [])
}
