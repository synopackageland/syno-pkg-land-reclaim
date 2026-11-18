import { formatBytes, formatCount, formatPercent, t } from '@/i18n'
import type { DisplayReport } from './reportCompleteness'

export function approximationNote(report?: DisplayReport | null): string {
  const a = report?.attribution
  if (!a?.approximation) return ''
  const bytes = a.approximation.unaccounted_bytes
  const amount = a.used_bytes != null && a.used_bytes > 0
    ? t('state.approximate.share', { percent: formatPercent(bytes / a.used_bytes) })
    : formatBytes(bytes)
  return t('state.approximate.note', {
    amount,
    changed: formatCount(a.approximation.changed_subvolumes),
    skipped: formatCount(a.approximation.skipped_roots),
  })
}
