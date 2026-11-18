<script setup lang="ts">
// Some volumes are Btrfs and some are not: the pages list only the Btrfs ones,
// so say which were skipped. Hidden when none were.
import { Callout } from '@synopackageland/ui'
import { t } from '@/i18n'
import { unsupportedVolumes } from '@/stores/volumes'
import UnsupportedVolumeList from './UnsupportedVolumeList.vue'
</script>

<template>
  <Callout v-if="unsupportedVolumes.length" tone="info" :title="t('state.unsupported.title')" data-testid="unsupported-volumes">
    <p>{{ t('state.unsupported.body') }}</p>
    <UnsupportedVolumeList :mounts="unsupportedVolumes" />
  </Callout>
</template>
