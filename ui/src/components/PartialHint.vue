<script setup lang="ts">
// A page whose own figures miss some data says so in one quiet line; the
// header's scan badge already carries the warning, so no callout repeats it.
import { TriangleAlert } from '@lucide/vue'
import { joinParts, locale, t } from '@/i18n'
defineProps<{ details: string[] }>()
</script>

<template>
  <p class="partial-hint" data-testid="partial-hint">
    <TriangleAlert class="icon" aria-hidden="true" />
    <span>{{ joinParts([t('state.partial.title'), ...details.filter(Boolean)], locale) }}</span>
  </p>
</template>

<style scoped>
.partial-hint {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  margin: 0;
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.icon {
  flex: none;
  width: 14px;
  height: 14px;
  margin-top: 2px;
  color: var(--ui-warning);
}
</style>
