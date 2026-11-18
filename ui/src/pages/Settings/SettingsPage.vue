<script setup lang="ts">
// Settings: the SDK's executor approval in the DSM
// window or its authorization row elsewhere, the SDK's Application Portal
// panel (DSM only) and the redacted support bundle. Cleanup stays allowed
// (a review that finds it turned off allows it again), the app follows the
// DSM language, and Package Center shows the version. The system partition
// warning can be turned off for this browser; its card still cleans up.
// One way to apply per page: the Application Portal panel applies its alias
// and entry actions itself (DSM reloads its web server for them), so the page
// has no Reset / Apply footer; the warning choice, a preference of this
// browser, is a Switch that takes effect at once. Each section explains itself
// under its title, and the download sits in the section footer.
import { computed, inject, ref } from 'vue'
import { Button, SettingsPage, SettingsRow, SettingsSection, Switch, useToast } from '@synopackageland/ui'
import { AuthorizationSettings, ExecutorSettings, ExecutorSetupKey, PortalSettings } from '@synopackageland/sdk/vue'
import { api } from '@/api'
import { locale, t } from '@/i18n'
import { portal } from '@/portal'
import { authorizeRootWork } from '@/stores/authorization'
import { preferences, setSystemPartitionWarning } from '@/stores/preferences'
import { selectedVolume } from '@/stores/volumes'
import PreferredShares from './PreferredShares.vue'

type BundleState = 'idle' | 'loading'
const toast = useToast()

// The warning choice is stored in this browser as soon as it changes.
const warning = computed({
  get: () => preferences.systemPartitionWarning,
  set: shown => setSystemPartitionWarning(shown),
})

// Only an administrator passes the package's entry gate to reach Settings.
const executor = inject(ExecutorSetupKey, undefined)

const bundleState = ref<BundleState>('idle')

const volume = computed(() => selectedVolume.value?.mount.path ?? null)

async function downloadBundle() {
  if (!volume.value || bundleState.value === 'loading') return
  bundleState.value = 'loading'
  try {
    if (!(await authorizeRootWork())) {
      bundleState.value = 'idle'
      return
    }
    const bundle = await api('support_bundle.get', { volume: volume.value })
    const url = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' }))
    const link = document.createElement('a')
    link.href = url
    link.download = `synoreclaim-support-${volume.value.replace(/[^\w]+/g, '')}.json`
    link.click()
    URL.revokeObjectURL(url)
    toast.success(t('settings.diagnostics.done'))
  } catch {
    toast.danger(t('settings.diagnostics.failed'))
  } finally {
    bundleState.value = 'idle'
  }
}
</script>

<template>
  <SettingsPage class="reclaim-settings">
    <ExecutorSettings v-if="executor" :admin="true" :locale="locale" :purpose="t('shell.purpose')" />
    <AuthorizationSettings v-else />
    <PortalSettings v-if="portal" :portal="portal" />
    <SettingsSection :collapsible="false" :title="t('settings.systemPartition.title')" :description="t('settings.systemPartition.description')">
      <SettingsRow :label="t('settings.systemPartition.warningLabel')">
        <Switch v-model="warning" :label="t('settings.systemPartition.warn')" />
      </SettingsRow>
    </SettingsSection>
    <PreferredShares />
    <SettingsSection :collapsible="false" :title="t('settings.diagnostics.title')" :description="t('settings.diagnostics.bundle.description')">
      <template #footer>
        <Button :disabled="!volume" :loading="bundleState === 'loading'" @click="downloadBundle">
          {{ t('settings.diagnostics.download') }}
        </Button>
        <span v-if="!volume" class="settings-hint">{{ t('settings.diagnostics.noVolume') }}</span>
      </template>
    </SettingsSection>
  </SettingsPage>
</template>

<style scoped>
/* One label column for the page, as DSM sets per page (Network: 238px), so
   the longest label (the portal link) stays on one line. */
.reclaim-settings {
  --ui-label-column: min(238px, 40%);
}
.settings-hint {
  color: var(--ui-text-secondary);
  font-size: var(--ui-font-size-sm);
}
</style>
