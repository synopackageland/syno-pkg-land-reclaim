<script lang="ts">
// The scan picture: three large, soft glows in the
// primary colour drifting and breathing behind the count, with a thin arc and no
// track when the scan has an estimate. Gentle mode drifts slowly, fast mode
// quickly; the two cross-fade. The glows brighten as the scan reads faster.
// Only transform and opacity move; it pauses while the page is hidden or the
// scan is paused, and stands still for reduced motion.
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import type { FieldDensity, FieldMode } from './scanFieldModel'

const MODES: FieldMode[] = ['gentle', 'fast']
const GLOWS = [1, 2, 3]
/** The layer fade; the old mode's layer goes once it has faded out. */
const FADE_MS = 700
/** The progress arc, near the edge of the 160 × 160 picture. */
const ARC_RADIUS = 77
const ARC_LENGTH = 2 * Math.PI * ARC_RADIUS
</script>

<script setup lang="ts">
const props = defineProps<{
  label: string
  mode: FieldMode
  density: FieldDensity
  /** 0–1 when the scan has an estimate; drawn as an arc around the glows. */
  ratio?: number | null
  paused?: boolean
}>()

const known = computed(() => props.ratio != null)
const arcOffset = computed(() => ARC_LENGTH * (1 - (props.ratio ?? 0)))

// The new mode's layer fades in over the old one, which then goes.
const shown = ref<FieldMode[]>([props.mode])
let fading: ReturnType<typeof setTimeout> | undefined
watch(() => props.mode, mode => {
  shown.value = MODES.filter(each => each === mode || shown.value.includes(each))
  clearTimeout(fading)
  fading = setTimeout(() => { shown.value = [props.mode] }, FADE_MS)
})

const hidden = ref(document.hidden)
const onVisibility = () => { hidden.value = document.hidden }
onMounted(() => document.addEventListener('visibilitychange', onVisibility))
onUnmounted(() => {
  document.removeEventListener('visibilitychange', onVisibility)
  clearTimeout(fading)
})
</script>

<template>
  <div
    class="scan-field" :class="{ paused: hidden || paused }" :data-density="density" role="progressbar" :aria-label="label"
    :aria-valuenow="known ? Math.round(ratio! * 100) : undefined" :aria-valuemin="known ? 0 : undefined" :aria-valuemax="known ? 100 : undefined"
  >
    <div class="field-glows">
      <div v-for="layer in shown" :key="layer" class="field-layer" :class="[layer, { on: layer === mode }]">
        <div v-for="glow in GLOWS" :key="glow" class="field-orbit" :data-glow="glow"><div class="field-glow" /></div>
      </div>
    </div>
    <svg v-if="known" class="field-arc" viewBox="0 0 160 160" aria-hidden="true">
      <circle class="field-value" cx="80" cy="80" :r="ARC_RADIUS" :stroke-dasharray="ARC_LENGTH" :stroke-dashoffset="arcOffset" />
    </svg>
  </div>
</template>

<style scoped>
.scan-field { position: relative; width: 100%; height: 100%; }
.field-glows { position: absolute; inset: 8%; transition: opacity 1.2s ease; }
/* Brighter as the scan reads faster. */
.scan-field[data-density="1"] .field-glows { opacity: 0.6; }
.scan-field[data-density="2"] .field-glows { opacity: 0.8; }
.scan-field[data-density="3"] .field-glows { opacity: 1; }
.field-layer { position: absolute; inset: 0; animation: field-in 700ms ease; transition: opacity 700ms ease; }
.field-layer:not(.on) { opacity: 0; }
.field-layer.gentle { --orbit: 22s; --breathe: 7s; --glow: 0.75; }
.field-layer.fast { --orbit: 5s; --breathe: 2.2s; --glow: 1; }
/* Each glow drifts on its own circle and breathes; none is ever a small shape. */
.field-orbit { position: absolute; inset: 0; animation: field-turn var(--orbit) linear infinite; }
.field-orbit[data-glow="2"] { animation-duration: calc(var(--orbit) * 1.35); animation-direction: reverse; }
.field-orbit[data-glow="3"] { animation-duration: calc(var(--orbit) * 1.7); }
.field-glow { position: absolute; width: 70%; height: 70%; border-radius: 50%; opacity: var(--glow); animation: field-breathe var(--breathe) ease-in-out infinite; }
[data-glow="1"] .field-glow { left: 2%; top: 0; background: radial-gradient(closest-side, color-mix(in srgb, var(--ui-primary) 55%, transparent), transparent); }
[data-glow="2"] .field-glow { right: 0; top: 22%; background: radial-gradient(closest-side, color-mix(in srgb, color-mix(in srgb, var(--ui-primary) 45%, #38bdf8) 55%, transparent), transparent); animation-delay: calc(var(--breathe) * -0.4); }
[data-glow="3"] .field-glow { left: 18%; bottom: 0; background: radial-gradient(closest-side, color-mix(in srgb, color-mix(in srgb, var(--ui-primary) 50%, #6366f1) 40%, transparent), transparent); animation-delay: calc(var(--breathe) * -0.7); }
.field-arc { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; }
/* No track: only the travelled part is drawn. Moves only when a poll brings a new fraction. */
.field-value { fill: none; stroke: var(--ui-primary); stroke-width: 2.5; stroke-linecap: round; transform: rotate(-90deg); transform-origin: 80px 80px; transition: stroke-dashoffset 250ms ease; }
.paused .field-layer, .paused .field-orbit, .paused .field-glow { animation-play-state: paused; }
@keyframes field-in {
  from { opacity: 0; }
}
@keyframes field-turn {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
@keyframes field-breathe {
  0%, 100% { transform: scale(0.82); }
  50% { transform: scale(1.12); }
}
/* Still: the glows rest where they start. */
@media (prefers-reduced-motion: reduce) {
  .field-layer, .field-orbit, .field-glow { animation: none; }
  .field-layer, .field-glows { transition: none; }
  .field-value { transition: none; }
}
</style>
