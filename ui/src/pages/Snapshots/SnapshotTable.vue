<script setup lang="ts">
// The snapshots of one target as a DSM-style list: a box per snapshot, its
// full time, DSM's lock state and what it holds alone. Shift-click selects a
// range; "older than" selects every snapshot before a day.
import { computed, ref } from 'vue'
import { Badge, Button, CellText, Select } from '@synopackageland/ui'
import { Lock, ShieldCheck } from '@lucide/vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import type { DsmSnapshotState } from '@/dsm/snapshotInventory'
import { formatBytes, formatDateTime, locale, t } from '@/i18n'
import type { SnapshotsDocument } from '@/types'
import { daysBefore, olderThan, rangeSelection } from './snapshotSelection'

type Snapshot = SnapshotsDocument['results'][number]['shares'][number]['snapshots'][number]

const props = defineProps<{
  modelValue: string[]
  /** Newest first. */
  snapshots: Snapshot[]
  /** DSM's state by snapshot name (time name or UUID); missing when DSM did not say. */
  states: Map<string, DsmSnapshotState> | null
  /** No snapshot of the target holds space alone: a 0 B row reads "shared". */
  sharedOnly: boolean
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string[]] }>()

const dayFormat = new Intl.DateTimeFormat(locale, { year: 'numeric', month: 'numeric', day: 'numeric' })
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1)

const rows = computed(() => props.snapshots.map(snapshot => {
  const state = props.states?.get(baseName(snapshot.name.path))
  return {
    id: String(snapshot.subvolume_id),
    otime: snapshot.otime,
    time: formatDateTime(snapshot.otime),
    dsmName: state?.name ?? '',
    locked: state?.locked ?? null,
    immutable: state?.immutable ?? null,
    stateKnown: typeof state?.locked === 'boolean' || state?.immutable === true,
    bytes: snapshot.materialized ? snapshot.exclusive_bytes : null,
    materialized: snapshot.materialized,
    pending: snapshot.pending_deletion,
  }
}))
type Row = (typeof rows.value)[number]
const order = computed(() => rows.value.map(row => row.id))
const selectable = (row: Record<string, unknown>) => !(row as Row).pending

const columns = [
  { key: 'time', label: t('snapshots.table.time'), minWidth: 120, wrap: true },
  { key: 'lock', label: t('snapshots.table.lock'), width: 60, align: 'center' as const },
  { key: 'bytes', label: t('snapshots.table.exclusive'), width: 96, align: 'right' as const },
]

/** The last box the user changed: the far end of a shift-click range. */
const anchor = ref<string | null>(null)
let shift = false
function track(event: MouseEvent | KeyboardEvent) {
  shift = event.shiftKey
}
function update(keys: string[]) {
  const next = rangeSelection(props.modelValue, keys, order.value, shift ? anchor.value : null, id => !rows.value.find(row => row.id === id)?.pending)
  shift = false
  anchor.value = next.anchor ?? anchor.value
  emit('update:modelValue', next.keys)
}

const olderValue = ref('')
const olderOptions = computed(() => daysBefore(props.snapshots).map(day => ({
  value: String(day),
  label: t('snapshots.table.olderThan', { date: dayFormat.format(new Date(day)) }),
})))
function selectOlder(value: string) {
  if (!value) return
  emit('update:modelValue', olderThan(props.snapshots, Number(value)))
  anchor.value = null
  olderValue.value = ''
}
function selectAll() {
  emit('update:modelValue', rows.value.filter(row => !row.pending).map(row => row.id))
}
function clear() {
  emit('update:modelValue', [])
  anchor.value = null
}
const rowLabel = (row: Record<string, unknown>) => t('snapshots.table.row', { time: String(row.time) })
</script>

<template>
  <div class="snapshot-table">
    <div class="tools">
      <Select
        v-if="olderOptions.length"
        :model-value="olderValue"
        class="older"
        :options="olderOptions"
        :placeholder="t('snapshots.table.olderPlaceholder')"
        :aria-label="t('snapshots.table.olderPlaceholder')"
        @update:model-value="selectOlder"
      />
      <Button size="sm" @click="selectAll">{{ t('snapshots.chart.selectAll') }}</Button>
      <Button size="sm" variant="ghost" :disabled="!modelValue.length" @click="clear">{{ t('snapshots.table.clear') }}</Button>
    </div>
    <div class="grid" @click.capture="track" @keydown.capture="track">
      <ReclaimGrid
        table-id="snapshot-list-v1"
        :columns="columns"
        :rows="rows"
        row-key="id"
        :page-size="1000"
        selectable
        select-all-scope="all"
        :selected-keys="modelValue"
        :row-selectable="selectable"
        :row-select-disabled-reason="() => t('snapshots.table.pending')"
        :select-row-label="rowLabel"
        :empty-label="t('snapshots.table.empty')"
        @update:selected-keys="update"
      >
        <template #cell-time="{ row }">
          <CellText :data-snapshot-key="String(row.id)" class="tabular-nums" wrap :text="String(row.time)" :secondary="row.dsmName ? String(row.dsmName) : undefined" />
        </template>
        <template #cell-lock="{ row }">
          <span class="lock">
            <ShieldCheck v-if="row.immutable" :size="16" class="immutable" role="img" :aria-label="t('snapshots.table.immutable')" :data-ui-tooltip-text="t('snapshots.table.immutable')" />
            <Lock v-else-if="row.locked" :size="16" role="img" :aria-label="t('snapshots.table.locked')" :data-ui-tooltip-text="t('snapshots.table.locked')" />
            <span v-else-if="!row.stateKnown" class="muted-dash" :data-ui-tooltip-text="t('snapshots.table.lockUnknown')" :aria-label="t('snapshots.table.lockUnknown')">–</span>
          </span>
        </template>
        <template #cell-bytes="{ row }">
          <Badge v-if="row.pending" tone="neutral">{{ t('snapshots.table.deleting') }}</Badge>
          <span v-else-if="!row.materialized" class="quiet">{{ t('snapshots.table.unread') }}</span>
          <span v-else-if="row.bytes == null || (sharedOnly && !row.bytes)" class="quiet">{{ t('snapshots.chart.sharedValue') }}</span>
          <CellText v-else class="tabular-nums" :text="formatBytes(row.bytes as number)" />
        </template>
      </ReclaimGrid>
    </div>
  </div>
</template>

<style scoped>
.snapshot-table { display: flex; min-width: 0; flex-direction: column; gap: 8px; }
.tools { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.older { width: 220px; max-width: 100%; }
/* At most this tall: the rows scroll under the header and footer that stay. */
.grid { display: flex; min-height: 0; max-height: 420px; flex-direction: column; }
.lock { display: inline-flex; align-items: center; justify-content: center; color: var(--ui-text-secondary); }
.lock .immutable { color: var(--ui-success); }
.muted-dash, .quiet { color: var(--ui-text-secondary); }
</style>
