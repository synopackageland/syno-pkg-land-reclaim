// Saved share reports (SPEC 5.1) supplement a missing or older whole-volume answer.
import type { HistoryDocument, Path, TopDocument } from '@/types'

const parentOf = (path: string) => path.slice(0, path.lastIndexOf('/'))

/** The shares of `mount` with a report from a completed share scan, in history order, once each. */
export function scannedShares(history: HistoryDocument, mount: string): Path[] {
  const shares = new Map<string, Path>()
  for (const scan of history.scans) {
    if (scan.volume || scan.report == null || scan.outcome !== 'completed') continue
    for (const target of scan.targets) {
      if (parentOf(target.path) === mount && !shares.has(target.path)) shares.set(target.path, target)
    }
  }
  return [...shares.values()]
}

/** Share answers published after the selected volume report must not be hidden by it. */
export function newerScannedShares(history: HistoryDocument, mount: string, finished?: string | null): Path[] {
  return scannedShares({ ...history, scans: history.scans.filter(scan =>
    !finished || Date.parse(scan.finished) > Date.parse(finished),
  ) }, mount)
}

/** The report finished first: what a page answering from several share reports is at least as old as. */
export function oldestReport<R extends { scan_finished: string }>(reports: R[]): R | null {
  return [...reports].sort((left, right) => Date.parse(left.scan_finished) - Date.parse(right.scan_finished))[0] ?? null
}

/** At most this many share reports load their root totals at a time (design review 2026-10-01 §1). */
export const SHARE_METRIC_PAGE = 50

/** The next share paths to fetch totals for: those not tried yet, at most one page. */
export function nextMetricTargets(paths: string[], attempted: (path: string) => boolean, page = SHARE_METRIC_PAGE): string[] {
  return paths.filter(path => !attempted(path)).slice(0, page)
}

type Root = TopDocument['results'][number]['target']
type HeldBy = TopDocument['results'][number]['entries'][number]['held_by'][number]

/** The holder class each blocker's bytes count in (the CLI's HeldClass::of); unknown is Metrics.unknown. */
const HOLDER_OF: Partial<Record<Root['blockers'][number], HeldBy['class']>> = {
  snapshot: 'snapshot',
  shared_tree: 'snapshot',
  pending_subvolume_deletion: 'pending_cleaner',
  orphan_inode: 'open_file',
  hard_link: 'hard_link',
  live_reflink: 'reflink',
  other_subvolume: 'reflink',
}

/**
 * Who holds the bytes deleting a report's root would not free. The CLI
 * lists holders for entries only, so a root's are known only when its
 * blockers name one holder class; null when they name several.
 */
export function heldByOfRoot(root: Root): HeldBy[] | null {
  const { physical, reclaimable, unknown } = root.metrics
  if (reclaimable == null) return null
  const retained = Math.max(0, physical - reclaimable - unknown)
  const classes = [...new Set(root.blockers.flatMap(blocker => HOLDER_OF[blocker] ?? []))]
  if (retained > 0 && classes.length !== 1) return null
  const held: HeldBy[] = [
    ...(retained > 0 ? [{ class: classes[0]!, bytes: retained }] : []),
    ...(unknown > 0 ? [{ class: 'unknown' as const, bytes: unknown }] : []),
  ]
  return held.sort((left, right) => right.bytes - left.bytes)
}
