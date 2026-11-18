<script setup lang="ts">
// The mounts Syno Reclaim skips because their file system is not Btrfs.
import { ListItem } from '@synopackageland/ui'
import { code } from '@/i18n'

defineProps<{ mounts: string[] }>()
</script>

<template>
  <div v-if="mounts.length" role="list">
    <ListItem v-for="mount in mounts" :key="mount" :title="mount" :description="code('error', 'unsupported_filesystem')" />
  </div>
</template>
