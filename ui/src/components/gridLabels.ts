import type { DataGridLabels } from '@synopackageland/ui'
import type { Translator } from '@/i18n/t'

/** All library strings use the app's translator and count formatter. */
export function gridLabels(t: Translator['t'], count: (value: number) => string): DataGridLabels {
  return {
    empty: t('common.grid.empty'),
    items: n => t('common.grid.items', { count: count(n) }),
    page: (page, total) => t('common.picker.page', { count: count(page), total: count(total) }),
    previous: t('common.grid.previous'),
    next: t('common.grid.next'),
    refresh: t('common.grid.refresh'),
    selectAll: t('common.grid.selectAll'),
    selectRow: key => t('common.grid.selectRow', { key }),
    resizeColumn: label => t('common.grid.resizeColumn', { label }),
  }
}
