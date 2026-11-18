<script setup lang="ts">
// The selected volume has no complete scan to compare usage with yet: a quiet
// row in the shell's status list, next to the scan that will make one.
import { ListItem } from '@synopackageland/ui'
import { formatVolume, t } from '@/i18n'
import { selectedVolume } from '@/stores/volumes'
</script>

<template>
  <ListItem v-if="selectedVolume" class="usage-hint" :title="formatVolume(selectedVolume.mount.path)" :description="t('usageChange.noBaseline')" />
</template>
