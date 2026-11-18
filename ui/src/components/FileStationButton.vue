<script setup lang="ts">
// The one way a list row opens its place in File Station: an icon button that,
// when File Station cannot show the row, stays visible, disabled, with the reason.
import { IconButton, Tooltip } from '@synopackageland/ui'
import { FolderOpen } from '@lucide/vue'
import { t } from '@/i18n'

defineProps<{
  /** Why File Station cannot show the row; empty when it can. */
  reason?: string
}>()
defineOptions({ inheritAttrs: false })
const emit = defineEmits<{ open: [] }>()
</script>

<template>
  <!-- A disabled button takes no pointer: its wrapper carries the reason. -->
  <Tooltip :content="reason || t('common.fileStation.open')">
    <span class="file-station-button" :tabindex="reason ? 0 : undefined" v-bind="$attrs" @click.stop>
      <IconButton
        size="sm"
        variant="ghost"
        :label="t('common.fileStation.open')"
        :disabled="!!reason"
        @click.stop="emit('open')"
      >
        <FolderOpen class="size-4" aria-hidden="true" />
      </IconButton>
    </span>
  </Tooltip>
</template>

<style scoped>
.file-station-button {
  display: inline-flex;
  flex-shrink: 0;
  border-radius: var(--ui-radius-sm);
  outline: none;
}
.file-station-button:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
</style>
