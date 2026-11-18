<script setup lang="ts">
import type { Component } from 'vue'
import { CellText, SizeBar, Tooltip } from '@synopackageland/ui'
import FileStationButton from '@/components/FileStationButton.vue'
import { formatBytes } from '@/i18n'

/** What a cell of the Explore list draws: the row, worked out once per list, not per draw. */
export type ExploreCellRow = {
  id: string
  path: string
  name: string
  icon: Component
  /** The name's second line in a wide list, and in a narrow one (with who holds it and where to manage it). */
  context: string
  compactContext: string
  /** Why File Station cannot show the row; empty when it can. */
  fileStation: string
  physical: number | null
  reclaimable: number | null
  held: string
  location: string
  locationLines: { place: string; action: string }
}

/**
 * One cell of the Explore list. It takes only values, so a list drawn again
 * (a dragged split, a resized window) leaves its cells alone; a cell's tooltip
 * and buttons are drawn again only when its row changes.
 */
const props = defineProps<{
  column: 'name' | 'physical' | 'reclaimable' | 'held' | 'location'
  row: ExploreCellRow
  /** A narrow list: the name's second line says who holds the space and where to manage it. */
  compact?: boolean
  /** The largest size in the list, the scale of the Size column's bars. */
  largest?: number
}>()

const emit = defineEmits<{ activate: [row: ExploreCellRow]; fileStation: [row: ExploreCellRow] }>()
</script>

<template>
  <div v-if="props.column === 'name'" class="explore-name">
    <button
      type="button"
      class="flex min-w-0 max-w-full flex-1 items-start gap-2 overflow-hidden rounded text-left outline-none focus-visible:shadow-[0_0_0_2px_var(--ui-focus-ring)]"
      :aria-label="row.path"
      :title="row.path"
      @click.stop="emit('activate', row)"
    >
      <CellText :icon="row.icon" :text="row.name" :secondary="(compact ? row.compactContext : row.context) || undefined" :wrap="!compact">
        <span class="text-[var(--ui-primary)]">{{ row.name }}</span>
      </CellText>
    </button>
    <FileStationButton :reason="row.fileStation" data-testid="explore-file-station" @open="emit('fileStation', row)" />
  </div>
  <div v-else-if="props.column === 'physical'" class="explore-size">
    <CellText class="tabular-nums" :text="formatBytes(row.physical)" />
    <SizeBar :value="row.physical" :max="largest ?? 0" :part="row.reclaimable" />
  </div>
  <CellText v-else-if="props.column === 'reclaimable'" class="tabular-nums" :text="formatBytes(row.reclaimable)">
    <span class="font-semibold text-[var(--ui-primary)]">{{ formatBytes(row.reclaimable) }}</span>
  </CellText>
  <Tooltip v-else-if="props.column === 'held'" :content="row.held">
    <CellText class="explore-held" :text="row.held" :aria-label="row.held" :wrap="!compact" />
  </Tooltip>
  <Tooltip v-else :content="row.location">
    <CellText :text="row.locationLines.place" :secondary="row.locationLines.action || undefined" :aria-label="row.location" :wrap="!compact" />
  </Tooltip>
</template>

<style scoped>
.explore-size {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 3px;
  width: 100%;
  min-width: 0;
}
.explore-name {
  display: flex;
  align-items: center;
  gap: 4px;
  width: 100%;
  min-width: 0;
}
</style>
