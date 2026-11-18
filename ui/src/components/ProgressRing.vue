<script setup lang="ts">
import ScanField from './ScanField.vue'
import type { FieldDensity, FieldMode } from './scanFieldModel'

withDefaults(defineProps<{
  /** 0–1 from progressRatio, drawn as an arc around the scan field; null draws the field alone. */
  ratio: number | null
  label: string
  mode?: FieldMode
  density?: FieldDensity
  /** The scan is paused: the field stands still. */
  paused?: boolean
}>(), { mode: 'gentle', density: 2, paused: false })
</script>

<template>
  <div class="progress-ring">
    <ScanField :label="label" :mode="mode" :density="density" :ratio="ratio" :paused="paused" />
    <div class="ring-center"><slot /></div>
  </div>
</template>

<style scoped>
.progress-ring { position: relative; width: 224px; height: 224px; flex: none; }
.ring-center { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; text-align: center; }
</style>
