<script setup lang="ts">
import { computed } from 'vue'
import { Button, Callout, Space } from '@synopackageland/ui'
import { formatBytes, formatVolume, joinParts, locale, t } from '@/i18n'
import { openNewScan } from '@/stores/nav'
import { actOnAlerts, usageAlertShown as visible, usageChange } from './useUsageChange'
const summary = computed(() => joinParts(usageChange.alerts.map(alert => t('usageChange.volume', {
  volume: formatVolume(alert.mount),
  message: t(alert.delta >= 0 ? 'usageChange.increase' : 'usageChange.decrease', { size: formatBytes(Math.abs(alert.delta)) }),
})), locale))
</script>

<template>
  <Callout v-if="visible" :dismissible="false" tone="warning" class="usage-banner" :title="summary">
    <template #actions>
      <Space wrap :spacing="8" align-items="stretch" class="usage-actions">
        <Button size="sm" @click="openNewScan">{{ t('usageChange.rescan') }}</Button>
        <Button size="sm" variant="ghost" @click="actOnAlerts('snooze')">{{ t('usageChange.snooze') }}</Button>
        <Button size="sm" variant="ghost" @click="actOnAlerts('dismiss')">{{ t('usageChange.close') }}</Button>
      </Space>
    </template>
  </Callout>
</template>
