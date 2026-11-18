// The quiet report notes every page folds into its "Notes" (ScanNotes):
// unattributed data above the small threshold, entries skipped because they
// changed during the scan, and changes only in DSM's own system areas. A
// coverage gap stays a banner (StateView partial), never a note.
import { formatBytes, t } from '@/i18n'
import { approximationNote } from './approximation'
import { changesOnlyInSystemAreas, type ConsistentReport, type ShareIdentity } from './consistency'
import { reportCompleteness, type DisplayReport } from './reportCompleteness'

export function reportNotes(report: (DisplayReport & ConsistentReport) | null | undefined, shares: readonly ShareIdentity[], completion?: string | null): string[] {
  if (!report) return []
  const c = reportCompleteness(report, completion)
  return [
    ...(c.unknownNote ? [approximationNote(report) || t('state.unattributed.note', { bytes: formatBytes(c.unknownBytes) })] : []),
    ...(c.churnEntries ? [t('state.namespaceChurn.note', { count: c.churnEntries })] : []),
    ...(changesOnlyInSystemAreas(report, shares) ? [t('common.consistency.systemAreasOnly')] : []),
  ]
}
