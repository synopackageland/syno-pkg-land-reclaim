<script setup lang="ts">
// A page's status sits in the page bar's meta (the SDK PageInfo), not among its
// actions: one info button named by the status, tinted in its tone, that opens
// the status in words, its details and the page's notes. The bubble is dark, so
// the status inside it is plain text; the button carries the tone.
import { computed } from 'vue'
import { PageInfo } from '@synopackageland/sdk/vue'

const props = defineProps<{
  /** The status in words; it names the button. */
  label: string
  tone?: string
  details?: string
  notes?: readonly string[]
}>()

const shown = computed(() => (props.notes ?? []).filter(Boolean))
</script>

<template>
  <PageInfo data-testid="page-status" class="page-status" :data-tone="tone" :label="label">
    <div class="page-status__body">
      <p class="page-status__title">{{ label }}</p>
      <p v-if="details" class="page-status__details">{{ details }}</p>
      <ul v-if="shown.length" class="page-status__notes">
        <li v-for="note in shown" :key="note">{{ note }}</li>
      </ul>
    </div>
  </PageInfo>
</template>

<style>
/* The button and its bubble live in the page bar and on the body, outside this component's scope. */
.page-status[data-tone='warning'] {
  color: var(--ui-warning-text);
}
.page-status[data-tone='danger'] {
  color: var(--ui-danger-text);
}
.page-status__body {
  display: grid;
  gap: 6px;
  justify-items: start;
}
.page-status__title {
  margin: 0;
  font-weight: 600;
}
.page-status__details {
  margin: 0;
  white-space: pre-line;
}
.page-status__notes {
  display: grid;
  gap: 4px;
  margin: 0;
  padding-left: 18px;
  list-style: disc;
  white-space: pre-line;
}
</style>
