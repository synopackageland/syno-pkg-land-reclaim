// What the Overview shows, computed from overview.get and why_not_freed.get.
// No formatting here: the page formats with t().
import { reportCompleteness, visibleConsistency } from '@/components/reportCompleteness'
import { progressRatio, scanJobOnVolume } from '@/components/scanProgress'
import { isActiveJob } from '@/stores/jobStates'
import { inPackageArea } from '@/pages/Packages/packages'
import type { Job, OverviewDocument, Volume, WhyNotFreedDocument } from '@/types'

export type OverviewResult = OverviewDocument['results'][number]
type PartKind = OverviewResult['parts'][number]['kind']
type Reason = WhyNotFreedDocument['results'][number]['reasons'][number]

const partsOf = (result: OverviewResult, kinds: PartKind[]) => result.parts.filter(p => kinds.includes(p.kind))

export type Segment = { key: string; value: number; label?: string; service?: string }

// Chart order, largest usual contributors first (mockup Dashboard).
const PART_ORDER: PartKind[] = [
  'share_live',
  'snapshot_only',
  'dsm_service',
  'system_other',
  'recycle_bin',
  'shared_between_folders',
  'pending_cleanup',
  'pinned_unreferenced',
  'unknown',
]

export type Category = PartKind | 'other_used' | 'free'
/** How sure an item's size is: measured, a known minimum, or a ceiling (an estimate or a best case). */
export type Bound = 'exact' | 'at_least' | 'up_to'
/** What the Overview can do about an item; `packages` opens the Packages page for the grouped small services. */
export type ItemAction = 'recycle' | 'snapshot' | 'cleanup' | 'package' | 'packages' | 'wait' | 'open_unlinked' | 'unresolved_references' | 'system_partition'

/**
 * One row of "Where the space went": the chart, its legend and the suggested
 * cleanup all render from one list of these.
 */
export type BreakdownItem = {
  /** Unique; the chart segment key and the list row id. */
  key: string
  category: Category | 'system_partition'
  /** A service code for DSM service data, `other` for its grouped small services, a path for a large path. */
  sub?: string
  bytes: number
  bound: Bound
  action?: ItemAction
  /** False for a row whose space is another volume's or already inside other items: never drawn or added. */
  inBreakdown: boolean
  shares: string[]
  service?: string
  manageInId?: string
  path?: string
  /** What `path` is ("file", "dir"), for large paths. */
  pathKind?: string
  /** Subvolumes the background cleanup still has to free. */
  count?: number | null
  /** The DSM service data colour of a split service: 0 for the largest. */
  shade?: number
}

/** A suggested cleanup row: a breakdown item, or the System partition row, whose size is unknown until its report loads. */
export type ActionRow = Omit<BreakdownItem, 'bytes'> & { bytes: number | null }

/** A DSM service at least this share of the used space gets its own chart segment and row… */
const SPLIT_SHARE = 0.01
/** …up to this many; the rest are "Other DSM service data". */
const SPLIT_MAX = 5

const sharesOf = (items: { share?: string; bytes: number }[]) =>
  [...items].filter(i => i.share && i.bytes > 0).sort((a, b) => b.bytes - a.bytes).map(i => i.share!)

type ServiceItem = { service: string; managed_by: string; manage_in_id: string; bytes: number }
const serviceItems = (result: OverviewResult) => (partsOf(result, ['dsm_service'])[0]?.items ?? []) as ServiceItem[]

const item = (category: BreakdownItem['category'], bytes: number, rest: Partial<BreakdownItem> = {}): BreakdownItem => ({
  key: category,
  category,
  bytes,
  bound: 'exact',
  inBreakdown: true,
  shares: [],
  ...rest,
})

/** DSM service data: its large services, then the rest as one group to open on the Packages page. */
function dsmItems(result: OverviewResult, bytes: number, used: number): BreakdownItem[] {
  const large = serviceItems(result)
    .filter(i => i.service !== 'other_dsm_service_data' && i.bytes > 0 && i.bytes >= used * SPLIT_SHARE)
    .sort((a, b) => b.bytes - a.bytes)
    .slice(0, SPLIT_MAX)
  if (!large.length) return [item('dsm_service', bytes, { action: 'packages' })]
  const split = large.map((i, shade) =>
    item('dsm_service', i.bytes, { key: `dsm_service:${i.service}`, sub: i.service, service: i.service, manageInId: i.manage_in_id, action: 'package', shade }),
  )
  const rest = Math.max(0, bytes - split.reduce((sum, i) => sum + i.bytes, 0))
  return rest > 0 ? [...split, item('dsm_service', rest, { key: 'dsm_service:other', sub: 'other', action: 'packages' })] : split
}

/** Shared-folder data: the report's large paths, then the rest. Their sizes are proven to be freed. */
function shareItems(result: OverviewResult, bytes: number): BreakdownItem[] {
  const paths = result.actions
    .filter(a => a.kind === 'cleanup' && a.reclaimable_bytes)
    .map(a => item('share_live', a.reclaimable_bytes!, {
      key: `cleanup:${a.path?.path ?? a.rank}`,
      sub: a.path?.path,
      path: a.path?.path,
      pathKind: a.path_kind,
      shares: a.share ? [a.share] : [],
      action: 'cleanup',
    }))
  const rest = Math.max(0, bytes - paths.reduce((sum, i) => sum + i.bytes, 0))
  return rest > 0 ? [...paths, item('share_live', rest)] : paths
}

function partItems(result: OverviewResult, kind: PartKind, reasons: Reason[] | null, used: number): BreakdownItem[] {
  const part = result.parts.find(p => p.kind === kind)
  const bytes = part?.bytes ?? 0
  if (bytes <= 0) return []
  switch (kind) {
    case 'share_live':
      return shareItems(result, bytes)
    case 'dsm_service':
      return dsmItems(result, bytes, used)
    case 'snapshot_only': {
      // Snapshots the scan did not read can hold more only-snapshot data (SPEC 5.14).
      const unread = (result.report.coverage.snapshots_not_materialized ?? 0) > 0
      return [item(kind, bytes, { bound: unread ? 'at_least' : 'exact', action: 'snapshot', shares: sharesOf(part!.items) })]
    }
    case 'recycle_bin':
      return [item(kind, bytes, { action: 'recycle', shares: sharesOf(part!.items) })]
    case 'pending_cleanup': {
      const pending = reasons?.find(r => r.code === 'pending_subvolume_cleanup')
      return [item(kind, bytes, { action: 'wait', count: pending?.code === 'pending_subvolume_cleanup' ? pending.count : null })]
    }
    case 'unknown': {
      // Compact coverage: too little to act on, and neutral in the chart.
      if (reportCompleteness(result.report).smallUnknown) return [item(kind, bytes, { key: 'small_unknown' })]
      return [item(kind, bytes, { action: 'unresolved_references' })]
    }
    default:
      return [item(kind, bytes)]
  }
}

/** Deleted files a program keeps open: their space is inside other items, so a row only. */
function openUnlinked(reasons: Reason[] | null): BreakdownItem[] {
  const reason = reasons?.find(r => r.code === 'open_unlinked')
  if (reason?.code !== 'open_unlinked') return []
  // The /proc estimate counts each open file's blocks, also those other files share.
  const [bytes, bound] = reason.processes ? [reason.processes.bytes_estimate, 'up_to' as const] : [reason.report_orphan_bytes, 'exact' as const]
  return bytes > 0 ? [item('unknown', bytes, { key: 'open_unlinked', action: 'open_unlinked', bound, inBreakdown: false })] : []
}

/**
 * Where the volume's space went, as one list: the nine mutually exclusive
 * report parts (DSM service data by service, large paths inside shared-folder
 * data), the df-used remainder they do not attribute, and free space. Each
 * byte is in one item; `why` adds the background cleanup count and open files.
 */
export function breakdown(result: OverviewResult, reasons: Reason[] | null): BreakdownItem[] {
  const df = result.space.df
  const attributed = result.parts.reduce((sum, p) => sum + p.bytes, 0)
  const used = df?.used_bytes ?? attributed
  const parts = PART_ORDER.flatMap(kind => partItems(result, kind, reasons, used))
  const otherUsed = df ? Math.max(0, df.used_bytes - attributed) : 0
  const tail = [item('other_used', otherUsed), item('free', df?.free_bytes ?? 0)].filter(i => i.bytes > 0)
  return [...parts, ...tail, ...openUnlinked(reasons)]
}

/** The chart's segments: DSM service data split as in the list, every other part whole. */
export function chartItems(items: BreakdownItem[]): BreakdownItem[] {
  const segments: BreakdownItem[] = []
  for (const i of items.filter(i => i.inBreakdown)) {
    const key = i.category === 'dsm_service' || i.key === 'small_unknown' ? i.key : i.category
    const whole = segments.find(s => s.key === key)
    if (whole) whole.bytes += i.bytes
    else segments.push(i.key === key ? { ...i } : item(i.category, i.bytes, { key, bound: i.bound }))
  }
  return segments
}

/** The suggested cleanup: the items that have an action, largest first. */
export function actionItems(items: BreakdownItem[]): BreakdownItem[] {
  return items.filter(i => i.action).sort((a, b) => b.bytes - a.bytes)
}

const FOLDER_SLICES = 7

/** "Where the space went" by shared folder: the largest shares, the rest as "other". */
export function byFolder(result: OverviewResult): Segment[] {
  const items = [...(partsOf(result, ['share_live'])[0]?.items ?? [])].sort((a, b) => b.bytes - a.bytes)
  const top = items.slice(0, FOLDER_SLICES).map(i => ({ key: `share:${i.share}`, value: i.bytes, label: i.share }))
  const rest = items.slice(FOLDER_SLICES).reduce((sum, i) => sum + i.bytes, 0)
  return rest > 0 ? [...top, { key: 'other', value: rest }] : top
}

/** Used and total bytes: the loaded report's df, so a card agrees with the donut; the volume list before that. */
export function volumeUsage(volume: Volume, result?: OverviewResult | null) {
  const { used_bytes: used, total_bytes: total } = result?.space.df ?? volume
  return { used, total }
}

export type VolumeStatus =
  | { kind: 'scanning' | 'paused'; percent: number | null }
  | { kind: 'no_report' }
  | { kind: 'scanned'; at: string; consistency: string | null }


/** The card chip: a job on the volume first, else the loaded report, else the volume's last scan. */
export function volumeStatus(volume: Volume, jobs: Job[], report?: OverviewResult['report'], now = Date.now()): VolumeStatus {
  const job = scanJobOnVolume(jobs, volume.mount.path, now)
  if (job) return { kind: isActiveJob(job) ? 'scanning' : 'paused', percent: progressRatio(job.progress) }
  if (report) return { kind: 'scanned', at: report.scan_finished, consistency: visibleConsistency(report) }
  if (!volume.reports || !volume.last_scan) return { kind: 'no_report' }
  return { kind: 'scanned', at: volume.last_scan, consistency: visibleConsistency({ consistency: volume.last_consistency, attribution: volume.last_attribution }) }
}

const PACKAGE_SLICES = 7

/** "Where the space went" by package: the largest packages of the "DSM service data" part, by the Packages page rule, the rest as "other". */
export function byPackage(result: OverviewResult): Segment[] {
  const items = serviceItems(result).filter(item => item.bytes > 0 && inPackageArea(item.service)).sort((a, b) => b.bytes - a.bytes)
  const top = items.slice(0, PACKAGE_SLICES).map(i => ({ key: `package:${i.service}`, value: i.bytes, label: i.managed_by, service: i.service }))
  const rest = items.slice(PACKAGE_SLICES).reduce((sum, i) => sum + i.bytes, 0)
  return rest > 0 ? [...top, { key: 'other', value: rest }] : top
}
