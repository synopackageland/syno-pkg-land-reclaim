<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { AppRoot, Button, ToastProvider } from '@synopackageland/ui'
import { locale, t } from '@/i18n'
import RootfsCleanupDialog from './RootfsCleanupDialog.vue'

const dialog = ref<InstanceType<typeof RootfsCleanupDialog>>()
const openOnLoad = new URLSearchParams(window.location.search).get('open') === '1'

function open() {
  void dialog.value?.open()
}

onMounted(() => {
  if (openOnLoad) open()
})
</script>

<template>
  <AppRoot class="rootfs-clean-preview" :lang="locale">
    <ToastProvider>
      <main class="rootfs-clean-preview__main">
        <Button variant="primary" @click="open">{{ t('rootfsClean.entry') }}</Button>
      </main>
      <RootfsCleanupDialog ref="dialog" />
    </ToastProvider>
  </AppRoot>
</template>

<style scoped>
.rootfs-clean-preview { min-height: 100%; background: var(--ui-surface-subtle); }
.rootfs-clean-preview__main { display: grid; min-height: 100vh; place-items: center; }
</style>
