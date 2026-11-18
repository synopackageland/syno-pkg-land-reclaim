// How a breakdown item looks wherever it is shown: the chart, its legend and
// the suggested cleanup use these, so an item has one colour, name and amount.
import { code, formatBytes, t } from '@/i18n'
import type { Bound, BreakdownItem } from './overview'

// UI palette tokens keep the categories in sync with the theme.
const CATEGORY_COLORS: Record<string, string> = {
  share_live: 'var(--ui-blue-9)',
  snapshot_only: 'var(--ui-blue-12)',
  dsm_service: 'var(--ui-green-9)',
  recycle_bin: 'var(--ui-orange-9)',
  shared_between_folders: 'var(--ui-blue-7)',
  pending_cleanup: 'var(--ui-orange-11)',
  pinned_unreferenced: 'var(--ui-gray-10)',
  unknown: 'var(--ui-red-9)',
  system_other: 'var(--ui-gray-10)',
  small_unknown: 'var(--ui-gray-8)',
  other_used: 'var(--ui-gray-8)',
  free: 'var(--ui-gray-6)',
}
/** DSM service data stays green: its largest services first, the grouped rest lightest. */
const DSM_SHADES = ['var(--ui-green-9)', 'var(--ui-green-11)', 'var(--ui-green-7)', 'var(--ui-green-12)', 'var(--ui-green-10)']
const DSM_OTHER = 'var(--ui-green-5)'

/** The item's chart colour; none for a row outside the breakdown. */
export function colorOf(item: Pick<BreakdownItem, 'key' | 'category' | 'shade' | 'inBreakdown'>): string | undefined {
  if (!item.inBreakdown) return undefined
  if (item.category === 'dsm_service' && item.shade !== undefined) return DSM_SHADES[item.shade % DSM_SHADES.length]
  if (item.key === 'dsm_service:other') return DSM_OTHER
  return CATEGORY_COLORS[item.key === 'small_unknown' ? item.key : item.category]
}

/** The item's name in the chart legend. */
export function labelOf(item: BreakdownItem): string {
  if (item.category === 'dsm_service') {
    if (item.service) return code('service', item.service)
    return item.sub === 'other' ? t('overview.breakdown.otherDsm') : code('part', 'dsm_service')
  }
  if (item.key === 'small_unknown') return t('overview.donut.small_unknown')
  if (item.category === 'free' || item.category === 'other_used') return t(`overview.donut.${item.category}`)
  return code('part', item.category)
}

/** The size with its bound, the same words everywhere: "1 GB", "At least 1 GB", "Up to 1 GB". */
export function amountOf(bytes: number | null, bound: Bound): string {
  if (bytes == null || bound === 'exact') return formatBytes(bytes)
  return t(`overview.bound.${bound}`, { bytes: formatBytes(bytes) })
}
