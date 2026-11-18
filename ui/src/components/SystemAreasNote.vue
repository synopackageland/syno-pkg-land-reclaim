<script setup lang="ts">
// A quiet line for a report whose only changes were DSM's own writes in
// system areas (SPEC §3 "changed-during-scan"): reassures
// that cleanup in shared folders is not affected. One shared wording for
// Overview, Explore, Recycle Bin and Snapshots.
import { computed } from 'vue'
import { changesOnlyInSystemAreas, type ConsistentReport, type ShareIdentity } from './consistency'
import { t } from '@/i18n'

const props = defineProps<{ report: ConsistentReport | null | undefined; shares: readonly ShareIdentity[] }>()

const show = computed(() => !!props.report && changesOnlyInSystemAreas(props.report, props.shares))
</script>

<template>
  <p v-if="show" class="system-areas-note">{{ t('common.consistency.systemAreasOnly') }}</p>
</template>

<style scoped>
.system-areas-note {
  margin: 0;
  font-size: 12px;
  color: var(--ui-text-muted);
}
</style>
