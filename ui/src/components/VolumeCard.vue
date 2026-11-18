<script setup lang="ts">
// One volume in the Overview's selector row: name,
// status chip, usage bar, one headline figure and, when the card leads somewhere,
// what selecting it does. Texts come formatted.
import { Badge, Progress, Tooltip } from '@synopackageland/ui'
import { ChevronRight } from '@lucide/vue'

defineProps<{
  name: string
  chipDetails?: string
  chip: string
  chipTone?: 'neutral' | 'info' | 'success' | 'warning' | 'danger'
  /** Colour of the usage bar; the chip tone does not imply it. */
  barTone?: 'primary' | 'warning' | 'danger'
  usedRatio: number
  usedText: string
  figureLabel: string
  figure: string
  selected?: boolean
  /** Above four volumes only the shown one is expanded; the rest show name, usage and status only. */
  compact?: boolean
  /** What selecting the card does, as a link-styled line: the card is the button. */
  action?: string
}>()
defineEmits<{ select: [] }>()
</script>

<template>
  <button type="button" class="volume-card" :class="{ selected, 'volume-card--compact': compact }" :aria-pressed="selected" @click="$emit('select')">
    <span class="volume-card-head">
      <span class="volume-card-name">{{ name }}</span>
      <Tooltip v-if="chipDetails" :content="chipDetails" side="bottom"><Badge :tone="chipTone ?? 'neutral'">{{ chip }}</Badge></Tooltip>
      <Badge v-else :tone="chipTone ?? 'neutral'">{{ chip }}</Badge>
    </span>
    <Progress :value="usedRatio * 100" :tone="barTone ?? 'primary'" :aria-label="usedText" />
    <span class="volume-card-used">{{ usedText }}</span>
    <span v-if="!compact" class="volume-card-figure">
      <span>{{ figureLabel }}</span>
      <strong>{{ figure }}</strong>
    </span>
    <span v-if="action && !compact" class="volume-card-action">
      {{ action }}<ChevronRight class="size-3.5" aria-hidden="true" />
    </span>
  </button>
</template>

<style scoped>
.volume-card {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 240px;
  padding: 12px 14px;
  text-align: left;
  border: 1px solid var(--ui-border);
  border-radius: var(--ui-radius-lg);
  background: var(--ui-surface);
  color: var(--ui-text);
  cursor: pointer;
}
.volume-card.selected {
  border-color: var(--ui-primary);
  box-shadow: 0 0 0 3px var(--ui-primary-soft);
}
.volume-card--compact {
  min-width: 180px;
}
.volume-card-head,
.volume-card-figure {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.volume-card-name {
  font-weight: 600;
}
.volume-card-used,
.volume-card-figure span {
  font-size: 12px;
  color: var(--ui-text-muted);
}
.volume-card-figure strong {
  font-variant-numeric: tabular-nums;
}
.volume-card-action {
  display: inline-flex;
  align-items: center;
  align-self: flex-end;
  gap: 2px;
  font-size: 12px;
  font-weight: 600;
  color: var(--ui-primary);
}
.volume-card:hover .volume-card-action {
  text-decoration: underline;
}
</style>
