<script setup lang="ts">
// Attribution-only unknown data above the small threshold: one muted line,
// not a banner. Reasons live in the tooltip.
import { computed } from 'vue'
import { Tooltip } from '@synopackageland/ui'
import { formatBytes, t } from '@/i18n'
import { approximationNote } from '@/components/approximation'
import type { DisplayReport } from '@/components/reportCompleteness'

const props = defineProps<{
  bytes: number
  /** Already localized, e.g. "Owner not found: 1 GB". Falls back to a general explanation. */
  reasons?: string[]
  report?: DisplayReport | null
}>()

const details = computed(() => (props.reasons?.length ? props.reasons.join('\n') : t('state.unattributed.details')))
const note = computed(() => approximationNote(props.report) || t('state.unattributed.note', { bytes: formatBytes(props.bytes) }))
</script>

<template>
  <p class="unattributed-note" data-testid="unattributed-note">
    <Tooltip :content="details"><span tabindex="0">{{ note }}</span></Tooltip>
  </p>
</template>

<style scoped>
.unattributed-note {
  margin: 0;
  font-size: 12px;
  color: var(--ui-text-muted);
}
.unattributed-note span {
  text-decoration: underline dotted;
  text-underline-offset: 2px;
  cursor: help;
}
</style>
