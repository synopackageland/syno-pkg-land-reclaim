<script setup lang="ts">
// Keep library behaviour central: the app supplies localisation, page state
// and best-effort per-table width persistence, never its own sort or resize.
import { computed, ref, watch } from 'vue'
import { DataGrid, type DataGridColumn, type DataGridColumnWidths } from '@synopackageland/ui'
import { formatCount, t } from '@/i18n'
import { gridLabels } from './gridLabels'

defineOptions({ inheritAttrs: false })
const props = withDefaults(defineProps<{
  tableId: string
  columns: DataGridColumn[]
  rows: Record<string, unknown>[]
  pageSize?: number
  emptyLabel?: string
  /** With `selectable`: the accessible name of a row's box. */
  selectRowLabel?: (row: Record<string, unknown>) => string
}>(), { pageSize: 50, emptyLabel: undefined, selectRowLabel: undefined })

const page = ref(1)
const storageKey = `synoreclaim.grid.${props.tableId}.widths`
function readWidths(): DataGridColumnWidths {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(storageKey) ?? '{}')
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {}
    return Object.fromEntries(Object.entries(value).filter(([, width]) =>
      typeof width === 'number' && Number.isFinite(width) && width >= 40 && width <= 4000,
    ))
  } catch { return {} }
}
const widths = ref(readWidths())
watch(widths, value => {
  try { localStorage.setItem(storageKey, JSON.stringify(value)) } catch { /* In-memory resizing still works. */ }
})
watch(() => props.rows.length, length => {
  page.value = Math.min(page.value, Math.max(1, Math.ceil(length / props.pageSize)))
})
const labels = computed(() => {
  const base = gridLabels(t, n => formatCount(n))
  const rowLabel = props.selectRowLabel
  return {
    ...base,
    ...(props.emptyLabel ? { empty: props.emptyLabel } : {}),
    ...(rowLabel ? { selectRow: (key: string, row?: Record<string, unknown>) => row ? rowLabel(row) : base.selectRow(key) } : {}),
  }
})
</script>

<template>
  <DataGrid
    v-bind="$attrs"
    v-model:page="page"
    v-model:column-widths="widths"
    :columns="columns"
    :rows="rows"
    :page-size="pageSize"
    :labels="labels"
    :loading-text="t('common.loading')"
    footer
    resizable
  >
    <template v-for="(_, name) in $slots" #[name]="slotProps">
      <slot :name="name" v-bind="slotProps ?? {}" />
    </template>
  </DataGrid>
</template>
