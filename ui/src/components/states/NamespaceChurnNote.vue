<script setup lang="ts">
// A few entries that changed while the scan ran and were skipped: one muted
// line, not a banner. Hidden when `count` is 0.
import { t } from '@/i18n'

defineProps<{ count: number }>()
</script>

<template>
  <p v-if="count > 0" class="namespace-churn-note" data-testid="namespace-churn-note">{{ t('state.namespaceChurn.note', { count: count }) }}</p>
</template>

<style scoped>
.namespace-churn-note {
  margin: 0;
  font-size: 12px;
  color: var(--ui-text-muted);
}
</style>
