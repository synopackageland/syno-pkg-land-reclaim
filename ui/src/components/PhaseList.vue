<script setup lang="ts">
import { computed } from 'vue'
import { Check } from '@lucide/vue'
import type { Job } from '@/types'
import { t } from '@/i18n'
import { PHASES, progressPhaseIndex } from './scanProgress'

const props = defineProps<{ job: Job; filesystem?: 'btrfs' | 'ext4' }>()
const phases = computed<readonly string[]>(() => props.filesystem === 'ext4' ? ['scan', 'aggregate', 'publish'] : PHASES)
const phase = computed(() => props.job.progress?.phase ?? props.job.phase)
const currentIndex = computed(() => props.filesystem === 'ext4' ? phases.value.indexOf(phase.value) : progressPhaseIndex(phase.value))
const finished = computed(() => ['completed', 'partial'].includes(props.job.state))
</script>

<template>
  <ol class="phase-list" :aria-label="t('scan.phases.title')">
    <li
      v-for="(name, index) in phases" :key="name" class="phase-row"
      :class="{ complete: finished || index < currentIndex, current: !finished && index === currentIndex }"
    >
      <span class="phase-marker" aria-hidden="true">
        <Check v-if="finished || index < currentIndex" class="size-4" />
        <span v-else>{{ index + 1 }}</span>
      </span>
      <span class="phase-copy">
        <span class="phase-name">{{ t(filesystem === 'ext4' && name === 'aggregate' ? 'scan.phase.ext4Aggregate' : `scan.phase.${name}`) }}</span>
        <span class="phase-status">{{ finished || index < currentIndex ? t('scan.phases.done') : index === currentIndex ? t('scan.phases.current') : t('scan.phases.waiting') }}</span>
      </span>
    </li>
  </ol>
</template>

<style scoped>
.phase-list { display: grid; gap: 15px; padding: 0; margin: 0; list-style: none; }
.phase-row { display: flex; align-items: center; gap: 12px; min-width: 0; color: var(--ui-text-muted); }
.phase-row.complete, .phase-row.current { color: var(--ui-text); }
.phase-marker { display: grid; place-items: center; width: 28px; height: 28px; flex: none; border-radius: 50%; background: var(--ui-surface-hover); font-size: 12px; font-weight: 700; }
.phase-row.complete .phase-marker { color: var(--ui-gray-1); background: var(--ui-success); }
.phase-row.current .phase-marker { color: var(--ui-primary-foreground); background: var(--ui-primary); box-shadow: 0 0 0 4px var(--ui-primary-soft); }
.phase-copy { display: grid; gap: 2px; min-width: 0; }
.phase-name { font-size: 14px; font-weight: 600; }
.phase-status { color: var(--ui-text-muted); font-size: 12px; }
</style>
