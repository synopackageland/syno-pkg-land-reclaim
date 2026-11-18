<script setup lang="ts">
// "Suggested cleanup": the breakdown items that have an action, so each row
// has the size, bound and colour of its part in "Where the space went". Each
// row has its type's icon, what it is, one short line on where or why (cut
// with a tooltip, so rows keep one height) and names its one action.
// What all package rows share is one note.
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, ref, type Component } from 'vue'
import { Button, CellText, Sheet } from '@synopackageland/ui'
import { Camera, CircleHelp, File, FileX, Folder, HardDrive, Hourglass, Info, Package, Trash2 } from '@lucide/vue'
import { code, formatBytes, joinSentences, locale, t } from '@/i18n'
import { navigate, openPackage } from '@/stores/nav'
import { inPackageArea } from '@/pages/Packages/packages'
import type { ActionRow, ItemAction } from './overview'
import { amountOf, colorOf, labelOf } from './breakdownView'

const props = defineProps<{ rows: ActionRow[] }>()
const emit = defineEmits<{ watch: []; systemPartition: [] }>()

type Action = { label: string; run: (row: ActionRow) => void }

const how = ref<ActionRow | null>(null)
const go = (page: string) => () => navigate(page)
const HOW: Action = { label: 'overview.action.how', run: row => (how.value = row) }
// Package data is freed in its package; the Packages page says how, for each one.
const PACKAGES: Action = { label: 'overview.action.packages', run: row => openPackage(row.service!, row.path ?? '') }

const ACTIONS: Record<ItemAction, Action> = {
  recycle: { label: 'overview.action.recycle', run: go('recycle') },
  snapshot: { label: 'overview.action.snapshots', run: go('snapshots') },
  cleanup: { label: 'overview.action.explore', run: go('explore') },
  package: HOW,
  packages: { label: 'overview.action.packages', run: go('packages') },
  open_unlinked: HOW,
  unresolved_references: HOW,
  wait: { label: 'overview.action.watch', run: () => emit('watch') },
  system_partition: { label: 'overview.action.cleanUp', run: () => emit('systemPartition') },
}

const actionOf = (row: ActionRow) => (row.action === 'package' && row.service && inPackageArea(row.service) ? PACKAGES : ACTIONS[row.action!])

const ICONS: Record<ItemAction, Component> = {
  recycle: Trash2,
  snapshot: Camera,
  cleanup: Folder,
  package: Package,
  packages: Package,
  open_unlinked: FileX,
  unresolved_references: CircleHelp,
  wait: Hourglass,
  system_partition: HardDrive,
}
const iconOf = (row: ActionRow) => (row.action === 'cleanup' && row.pathKind === 'file' ? File : ICONS[row.action!])

function title(row: ActionRow) {
  if (row.action === 'package' || row.action === 'packages') return labelOf({ ...row, bytes: row.bytes ?? 0 })
  if (row.action === 'cleanup' && row.pathKind === 'file') return t('overview.row.cleanup.titleFile')
  return t(`overview.row.${row.action}.title`)
}

function why(row: ActionRow) {
  const shares = row.shares.join(', ')
  switch (row.action) {
    case 'recycle':
    case 'snapshot':
      return t(`overview.row.${row.action}.${shares ? 'whyShares' : 'why'}`, { shares })
    case 'cleanup':
      return row.path ?? shares
    case 'package':
      return t('overview.row.package.where', { place: code('manage_in', row.manageInId ?? 'other') })
    case 'system_partition':
      // The size comes from the rootfs report; without it, state the consequence only.
      if (row.bytes == null) return t('rootfs.level.full.body')
      return t(row.bound === 'up_to' ? 'rootfsView.row.whyUpTo' : 'rootfsView.row.why', { bytes: formatBytes(row.bytes) })
    case 'wait':
      return row.count ? t('overview.row.pending_subvolume_cleanup.why', { count: row.count }) : t('overview.row.wait.why')
    case 'open_unlinked':
      return [t('overview.row.open_unlinked.why'), t('overview.tag.frees_after_restart')].join(' · ')
    default:
      return t(`overview.row.${row.action}.why`)
  }
}

// The donut counts data a package shares once; Packages shows its whole size (seen on a test NAS).
const packageNote = computed(() =>
  props.rows.some(row => row.action === 'package' || row.action === 'packages') ? joinSentences([t('overview.row.package.note'), t('overview.row.package.counted')], locale) : '',
)

const columns = computed(() => [
  { key: 'title', sortable: true, label: t('overview.col.item'), minWidth: 160, tooltip: false },
  { key: 'size', sortable: true, label: t('overview.col.size'), align: 'right' as const, width: '120px' },
  { key: 'action', label: t('overview.col.action'), align: 'right' as const, width: '168px' },
])
const tableRows = computed(() =>
  props.rows.map(row => ({ id: row.key, row, title: title(row), secondary: why(row), size: row.bytes })),
)
const rowOf = (value: unknown) => (value as { row: ActionRow }).row

const howText = computed(() => {
  const row = how.value
  if (!row) return ''
  if (row.action !== 'package') return t(`overview.how.${row.action}`)
  return t('overview.how.package', { service: title(row), place: code('manage_in', row.manageInId ?? 'other') })
})
</script>

<template>
  <ReclaimGrid :fill="false" table-id="overview-actions" :columns="columns" :rows="tableRows" :empty-label="t('overview.actions.empty')">
    <template #cell-title="{ row, value }">
      <span class="item">
        <span class="swatch" :style="{ background: colorOf(rowOf(row)) }" aria-hidden="true" />
        <CellText class="item-text" :icon="iconOf(rowOf(row))" :text="String(value)" :secondary="String(row.secondary) || undefined" />
      </span>
    </template>
    <template #cell-size="{ row }">
      <span class="size tabular-nums">{{ amountOf(rowOf(row).bytes, rowOf(row).bound) }}</span>
    </template>
    <template #cell-action="{ row }">
      <Button size="sm" variant="ghost" class="action" @click="actionOf(rowOf(row)).run(rowOf(row))">
        {{ t(actionOf(rowOf(row)).label) }}
      </Button>
    </template>
  </ReclaimGrid>
  <p v-if="packageNote" class="table-note" data-testid="package-note"><Info class="size-3.5" aria-hidden="true" />{{ packageNote }}</p>
  <Sheet :aria-describedby="undefined" :open="how !== null" :title="t('overview.how.title')" :close-label="t('common.close')" @update:open="open => !open && (how = null)">
    <p>{{ howText }}</p>
  </Sheet>
</template>

<style scoped>
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.item-text {
  min-width: 0;
}
/* The row's legend swatch in "Where the space went"; empty for a row outside it. */
.swatch {
  flex: none;
  width: 10px;
  height: 10px;
  border-radius: 2px;
}
.size {
  white-space: nowrap;
}
.action {
  white-space: nowrap;
}
.table-note {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  margin: 8px 0 0;
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.table-note svg {
  flex: none;
  margin-top: 2px;
}
</style>
