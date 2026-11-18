<script setup lang="ts">
// Wherever a shared folder without a Recycle Bin is named: opens its row in
// DSM Control Panel › Shared Folder, where Edit turns the Recycle Bin on.
// Recheck reads the setting again afterwards. In a list row it is an icon,
// so the row keeps its height; its tooltip and label name the share.
import { ref } from 'vue'
import { Button, IconButton, Tooltip, useToast } from '@synopackageland/ui'
import { Settings } from '@lucide/vue'
import { t } from '@/i18n'
import { openShareSettings } from './shareSettings'

const props = defineProps<{
  share: string
  /** Name the share on the button: several buttons stand side by side. */
  named?: boolean
  /** An icon button, for a list row. */
  icon?: boolean
}>()
const toast = useToast({ optional: true })
const opening = ref(false)
async function open() {
  opening.value = true
  try {
    if (!await openShareSettings(props.share)) toast?.info(t('common.shareSettings.failed'))
  } finally {
    opening.value = false
  }
}
</script>

<template>
  <Tooltip v-if="icon" :content="t('common.shareSettings.openNamed', { share })">
    <span class="share-settings-icon" @click.stop>
      <IconButton size="sm" variant="ghost" :label="t('common.shareSettings.openNamed', { share })" :data-testid="`share-settings-${share}`" @click.stop="open">
        <Settings class="size-4" aria-hidden="true" />
      </IconButton>
    </span>
  </Tooltip>
  <Button v-else size="sm" variant="secondary" :loading="opening" :data-testid="`share-settings-${share}`" @click.stop="open">{{ named ? t('common.shareSettings.openNamed', { share }) : t('common.shareSettings.open') }}</Button>
</template>

<style scoped>
.share-settings-icon {
  display: inline-flex;
  flex-shrink: 0;
}
</style>
