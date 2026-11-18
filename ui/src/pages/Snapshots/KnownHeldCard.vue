<script setup lang="ts">
// What the report already says snapshots hold, row by row (SPEC §4.5): shown when
// it has no per-snapshot data, or when no snapshot holds space by itself.
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { CellText, EmptyState, Statistic, Surface } from '@synopackageland/ui'
import { Camera } from '@lucide/vue'
import { formatBytes, t } from '@/i18n'
import type { SnapshotHeld } from './knownHeld'

defineProps<{ known: SnapshotHeld }>()

const knownColumns = [
  { key: 'name', sortable: true, label: t('snapshots.known.column.name'), minWidth: 160 },
  { key: 'snapshotBytes', sortable: true, label: t('snapshots.known.column.held'), align: 'right' as const, width: '140px' },
]
</script>

<template>
  <Surface data-testid="snapshot-known" class="known-card" padding="md">
    <div class="card-title known-title">
      <strong>{{ t('snapshots.known.title') }}</strong>
      <span class="hint">{{ t('snapshots.known.description') }}</span>
    </div>
    <template v-if="known.rows.length">
      <Statistic
        class="known-total"
        :label="t('snapshots.known.total')"
        :value="known.lowerBound ? t('snapshots.known.atLeast', { bytes: formatBytes(known.total) }) : formatBytes(known.total)"
      />
      <ReclaimGrid :fill="false" table-id="snapshot-known" :columns="knownColumns" :rows="known.rows" row-key="key" :empty-label="t('snapshots.known.none')">
        <template #cell-name="{ row }">
          <CellText :text="String(row.name)" :secondary="row.name === row.folder ? undefined : String(row.folder)" :data-ui-tooltip-text="String(row.path)" wrap />
        </template>
        <template #cell-snapshotBytes="{ row }"><CellText class="tabular-nums" :text="formatBytes(row.snapshotBytes as number)" /></template>
      </ReclaimGrid>
    </template>
    <EmptyState v-else :title="t('snapshots.known.none')" :icon="Camera" />
  </Surface>
</template>

<style scoped>
.card-title {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 10px;
}
.card-title strong {
  font-size: var(--ui-font-size-lg);
}
.hint {
  font-size: var(--ui-font-size);
  color: var(--ui-text-secondary);
}
.known-title {
  flex-wrap: wrap;
}
.known-total {
  padding-bottom: 10px;
}
</style>
