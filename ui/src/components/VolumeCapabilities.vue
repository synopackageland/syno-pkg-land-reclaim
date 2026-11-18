<script setup lang="ts">
// A quiet line for a volume this app can scan but not clean up: what still
// works first, the reason and the way to free space behind "Why?".
import { joinParts, locale, t } from '@/i18n'
import type { Volume } from '@/types'
import { cleanupUnavailableReason } from './cleanupUnavailable'
defineProps<{ volume: Volume }>()
</script>

<template>
  <div v-if="!volume.capabilities.cleanup" class="cleanup-note" data-testid="cleanup-unavailable">
    <p>{{ joinParts([t('state.cleanupUnavailable'), t('state.cleanupUnavailable.still')], locale) }}</p>
    <details>
      <summary>{{ t('state.cleanupUnavailable.why') }}</summary>
      <p>{{ joinParts([cleanupUnavailableReason(volume), t('state.cleanupUnavailable.how')], locale) }}</p>
    </details>
  </div>
</template>

<style scoped>
.cleanup-note {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  column-gap: 8px;
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.cleanup-note p {
  margin: 0;
}
.cleanup-note details {
  flex-basis: 100%;
}
.cleanup-note summary {
  width: fit-content;
  cursor: pointer;
  border-radius: var(--ui-radius-md);
  color: var(--ui-primary);
  outline: none;
}
.cleanup-note summary:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
</style>
