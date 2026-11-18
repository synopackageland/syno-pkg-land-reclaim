// Recycle Bin page rules: share targets, oldest-item
// limits and the "empty older than" selection. The CLI returns top-level
// items already sorted by ctime, but the page sorts again so a hook or a
// fixture that is out of order still renders correctly.
import { oldestReport } from '@/components/shareReports'
import type { Path, RecycleDocument } from '@/types'

export type RecycleShare = RecycleDocument['shares'][number]
export type OldestItem = NonNullable<RecycleShare['oldest']>[number]

/** How many oldest items one fetch asks for, and the CLI's maximum. */
export const OLDEST_PAGE = 200
export const OLDEST_MAX = 1000

/** The "empty older than" choices of SPEC §4.7. */
export const AGE_DAYS = [7, 30, 90, 365] as const

/** The recycle command's target for a share: the mount plus its relative name. */
export function shareTarget(mount: string, share: Path): string {
  if (share.path.startsWith('/')) return share.path
  return `${mount.replace(/\/+$/, '')}/${share.path.replace(/^\/+/, '')}`
}

/** A per-user homes bin belongs to the homes share, the CLI's accepted scope. */
export function recycleTarget(mount: string, share: Path): string {
  const target = shareTarget(mount, share)
  const homes = `${mount.replace(/\/+$/, '')}/homes`
  return target.startsWith(`${homes}/`) ? homes : target
}

/** The next --oldest limit: one jump to the CLI maximum, then no more. */
export function nextOldestLimit(current: number): number {
  return current < OLDEST_MAX ? OLDEST_MAX : current
}

/** True while items are still missing and the limit can grow. */
export function canLoadMore(total: number, loaded: number, limit: number): boolean {
  return loaded < total && limit < OLDEST_MAX
}

/** Oldest first; items with no ctime estimate sort last (SPEC §4.7). */
export function sortOldest(items: OldestItem[]): OldestItem[] {
  const time = (item: OldestItem) => (item.ctime == null ? Infinity : Date.parse(item.ctime))
  return [...items].sort((a, b) => time(a) - time(b) || a.path.path.localeCompare(b.path.path))
}

/** The top-level items moved more than `days` ago; a missing ctime is never selected. */
export function olderThan(items: OldestItem[], days: number, now = Date.now()): OldestItem[] {
  const cutoff = now - days * 86_400_000
  return items.filter(item => item.ctime != null && Date.parse(item.ctime) < cutoff)
}

/**
 * The volume's no_report answer rebuilt from its shares' own answers (the
 * shares scanned on their own): the shares of every answer with a report,
 * the oldest report info, and each warning once.
 */
export function shareAnswersAsVolume(volume: RecycleDocument, answers: (RecycleDocument | null)[]): RecycleDocument {
  const reported = answers.filter((answer): answer is RecycleDocument => answer?.report_info != null)
  const documents = volume.report_info ? [volume, ...reported] : reported
  const warnings = new Map(documents.flatMap(answer => answer.warnings).map(warning => [warning.code, warning]))
  const shares = new Map<string, { finished: number; share: RecycleShare }>()
  for (const document of documents) for (const listed of document.shares) {
    const key = shareTarget(volume.target?.path ?? '', listed.share)
    // A row the CLI took from a newer share report carries that report.
    const own = listed.report_info ?? document.report_info!
    const finished = Date.parse(own.scan_finished)
    // Beside a volume report, a share's own answer says which report it came from.
    const share = volume.report_info && document !== volume && !listed.report_info ? { ...listed, report_info: own } : listed
    if (!shares.has(key) || finished >= shares.get(key)!.finished) shares.set(key, { finished, share })
  }
  const errors = documents.flatMap(document => document.errors)
  return {
    ...volume,
    completion: errors.length || documents.some(document => document.completion !== 'complete') ? 'partial' : 'complete',
    errors,
    warnings: [...warnings.values()],
    report_info: oldestReport(documents.map(answer => answer.report_info!)),
    shares: [...shares.values()].map(entry => entry.share),
  }
}
