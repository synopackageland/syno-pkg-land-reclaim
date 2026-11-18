<script setup lang="ts">
// A quiet, non-blocking status line for coverage, attribution and report
// warnings. One muted sentence; the reasons stay available in a
// focusable tooltip, so keyboard, touch and screen-reader users can read the
// full explanation without an amber panel.
import { computed } from 'vue'
import { Tooltip } from '@synopackageland/ui'

const props = defineProps<{
  /** Already localized, e.g. "Some data could not be read". */
  label: string
  /** Already localized reasons, one per line in the tooltip. */
  details?: string[]
  /** Test hook for pages that pin the note in place. */
  testid?: string
}>()

const details = computed(() => (props.details ?? []).filter(Boolean).join('\n'))
</script>

<template>
  <p class="status-note" :data-testid="testid ?? 'status-note'">
    <Tooltip v-if="details" :content="details">
      <span class="status-note__trigger" tabindex="0">{{ label }}</span>
    </Tooltip>
    <span v-else>{{ label }}</span>
  </p>
</template>

<style scoped>
.status-note {
  margin: 0;
  font-size: 12px;
  color: var(--ui-text-muted);
}
.status-note__trigger {
  text-decoration: underline dotted;
  text-underline-offset: 2px;
  cursor: help;
}
</style>
