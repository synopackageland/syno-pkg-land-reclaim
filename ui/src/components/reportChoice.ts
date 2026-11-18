// Which report a page answers from.
// Each path is answered by the newest report that covers it; at a volume
// root, shares scanned on their own after the volume scan answer from their
// own newer reports. The user can instead choose one saved report.
import { t } from '@/i18n'
import type { HistoryDocument } from '@/types'

export type ScanRecord = HistoryDocument['scans'][number]

/** The Select value for "the newest report of each path". */
export const NEWEST_REPORT = 'newest'

const within = (outer: string, path: string) =>
  path === outer || path.startsWith(outer.endsWith('/') ? outer : `${outer}/`)

/** Whether a scan's report covers `path`: a volume scan covers its volume, a path scan its targets. */
export function coversPath(scan: ScanRecord, mount: string, path: string): boolean {
  if (scan.volume) return scan.targets.some(target => target.path === mount) && within(mount, path)
  return scan.targets.some(target => within(target.path, path))
}

/** The saved reports covering `path` on `mount`, newest first, as the page can answer from them. */
export function reportsCovering(history: HistoryDocument | null, mount: string, path: string): ScanRecord[] {
  return (history?.scans ?? [])
    .filter(scan => scan.report != null && coversPath(scan, mount, path))
    .sort((left, right) => Date.parse(right.finished) - Date.parse(left.finished))
}

/** What a scan read, in words: the whole volume, one shared folder, or some folders. */
export type ReportScope = { key: 'volume' } | { key: 'share'; name: string } | { key: 'paths'; count: number }

export function reportScope(scan: Pick<ScanRecord, 'volume' | 'targets'>, mount: string): ReportScope {
  if (scan.volume) return { key: 'volume' }
  const prefix = `${mount.replace(/\/+$/, '')}/`
  const [only] = scan.targets
  if (scan.targets.length === 1 && only?.path.startsWith(prefix) && !only.path.slice(prefix.length).includes('/'))
    return { key: 'share', name: only.path.slice(prefix.length) }
  return { key: 'paths', count: scan.targets.length }
}

/** A scan's scope in words: "Whole volume", "Shared folder photos", "2 folders". */
export function scopeText(scan: Pick<ScanRecord, 'volume' | 'targets'>, mount: string): string {
  const scope = reportScope(scan, mount)
  if (scope.key === 'volume') return t('report.scope.volume')
  if (scope.key === 'share') return t('report.scope.share', { name: scope.name })
  return t('report.scope.paths', { count: scope.count })
}
