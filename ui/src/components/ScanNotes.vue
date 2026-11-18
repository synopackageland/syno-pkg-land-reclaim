<script setup lang="ts">
// Secondary explanations folded behind one "Notes (n)" line, so the page
// keeps only its key figures and next step in view. Readable contrast, not
// the muted caption colour.
import { computed } from 'vue'
import { t } from '@/i18n'

const props = defineProps<{ notes: string[] }>()
const shown = computed(() => props.notes.filter(Boolean))
</script>

<template>
  <details v-if="shown.length" class="scan-notes" data-testid="scan-notes">
    <summary>{{ t('common.notes', { count: shown.length }) }}</summary>
    <ul>
      <li v-for="note in shown" :key="note">{{ note }}</li>
    </ul>
  </details>
</template>

<style scoped>
.scan-notes {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.scan-notes summary {
  width: fit-content;
  cursor: pointer;
  border-radius: var(--ui-radius-md);
  color: var(--ui-text-secondary);
  outline: none;
}
.scan-notes summary:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
.scan-notes ul {
  display: grid;
  gap: 4px;
  margin: 6px 0 0;
  padding-left: 18px;
  list-style: disc;
  white-space: pre-line;
}
</style>
