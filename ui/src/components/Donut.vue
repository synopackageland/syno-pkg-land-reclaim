<script setup lang="ts">
// Plain SVG donut and its legend. Hovering or focusing
// an arc or its legend row highlights both and shows that part's name, amount
// and shares in the centre, which otherwise holds the slot (the total). The
// arcs are one tab stop; the arrow keys move between them.
import { computed, nextTick, ref, watch } from 'vue'
import { ChartLegend } from '@synopackageland/ui'
import { donutArcs } from './charts'

export type DonutSegment = {
  key: string
  value: number
  color: string
  label: string
  /** The formatted value, e.g. "15.8 GB". */
  amount: string
  /** The legend's percentage next to the amount. */
  share?: string
  /** Lines under the amount while the part is highlighted, e.g. "11.3% of used". */
  details?: string[]
}

const props = withDefaults(defineProps<{ segments: DonutSegment[]; title: string; size?: number; thickness?: number }>(), {
  size: 200,
  thickness: 24,
})
defineSlots<{ default?: () => unknown; label?: (props: { segment: DonutSegment }) => unknown }>()

const radius = 80
const circumference = 2 * Math.PI * radius
// About 4°: the smallest part still shows as an arc; the gap keeps neighbours apart.
const MIN_ARC = circumference / 90
const GAP = 1.5

const arcs = computed(() => {
  const geometry = donutArcs(props.segments.map(s => s.value), circumference, { minLength: MIN_ARC })
  const gap = props.segments.filter(s => s.value > 0).length > 1 ? GAP : 0
  return props.segments.map((s, i) => ({ ...s, offset: geometry[i].offset, drawn: Math.max(0, geometry[i].length - gap) }))
})

const activeKey = ref<string | null>(null)
const active = computed(() => props.segments.find(s => s.key === activeKey.value) ?? null)
const focusKey = computed(() => active.value?.key ?? props.segments[0]?.key)
// Other parts (another tab) clear the highlight.
watch(() => props.segments.map(s => s.key).join('\n'), () => (activeKey.value = null))
const legend = computed(() => props.segments.map(s => ({ key: s.key, label: s.label, color: s.color, value: s.amount, share: s.share })))
const segmentOf = (key: string) => props.segments.find(s => s.key === key)!
const describe = (s: DonutSegment) => [s.label, s.amount, ...(s.details ?? [])].join(', ')

const svg = ref<SVGSVGElement | null>(null)
async function move(from: number, key: string) {
  const n = props.segments.length
  const to = key === 'Home' ? 0 : key === 'End' ? n - 1 : (from + (key === 'ArrowRight' || key === 'ArrowDown' ? 1 : -1) + n) % n
  activeKey.value = props.segments[to].key
  await nextTick()
  svg.value?.querySelectorAll<SVGCircleElement>('circle')[to]?.focus()
}
function onKey(event: KeyboardEvent, index: number) {
  if (['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    void move(index, event.key)
  } else if (event.key === 'Escape') {
    activeKey.value = null
  }
}
function onFocusOut(event: FocusEvent) {
  if (!svg.value?.contains(event.relatedTarget as Node | null)) activeKey.value = null
}
</script>

<template>
  <div class="donut-chart">
    <div class="donut" :style="{ width: `${size}px`, height: `${size}px` }">
      <svg
        ref="svg"
        :width="size"
        :height="size"
        viewBox="0 0 200 200"
        role="group"
        :aria-label="title"
        @mouseleave="activeKey = null"
        @focusout="onFocusOut"
      >
        <circle
          v-for="(arc, i) in arcs"
          :key="arc.key"
          :class="{ active: arc.key === active?.key, dimmed: active !== null && arc.key !== active.key }"
          cx="100"
          cy="100"
          :r="radius"
          fill="none"
          :stroke="arc.color"
          :stroke-width="arc.key === active?.key ? thickness + 6 : thickness"
          :stroke-dasharray="`${arc.drawn} ${circumference}`"
          :stroke-dashoffset="arc.offset"
          transform="rotate(-90 100 100)"
          role="img"
          :aria-label="describe(arc)"
          :tabindex="arc.key === focusKey ? 0 : -1"
          @mouseenter="activeKey = arc.key"
          @focus="activeKey = arc.key"
          @keydown="onKey($event, i)"
        />
      </svg>
      <div class="donut-centre" :style="{ '--hole': `${((radius - thickness / 2 - 4) / 100) * size}px` }">
        <template v-if="active">
          <span class="centre-label">{{ active.label }}</span>
          <strong class="num">{{ active.amount }}</strong>
          <span v-for="line in active.details ?? (active.share ? [active.share] : [])" :key="line" class="centre-detail">{{ line }}</span>
        </template>
        <slot v-else />
      </div>
    </div>
    <!-- The SDK legend: a value never truncates and moves under its name when both do not fit. -->
    <ChartLegend class="legend" :items="legend" :active="active?.key ?? null" @update:active="activeKey = $event">
      <template #label="{ item }"><slot name="label" :segment="segmentOf(item.key)">{{ item.label }}</slot></template>
    </ChartLegend>
  </div>
</template>

<style scoped>
.donut-chart {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: center;
  gap: 24px;
}
.donut {
  position: relative;
  flex: none;
}
circle {
  cursor: default;
  outline: none;
  transition:
    opacity 0.12s,
    stroke-width 0.12s;
}
circle.dimmed {
  opacity: 0.35;
}
circle:focus-visible {
  outline: none;
  filter: drop-shadow(0 0 2px var(--ui-primary));
}
.donut-centre {
  position: absolute;
  top: 50%;
  left: 50%;
  width: calc(var(--hole) * 0.84);
  max-height: var(--hole);
  transform: translate(-50%, -50%);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 1px;
  overflow: hidden;
  text-align: center;
  overflow-wrap: anywhere;
  pointer-events: none;
}
.centre-label {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  font-size: var(--ui-font-size-sm);
}
.centre-detail {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.legend {
  flex: 1 1 260px;
  font-size: var(--ui-font-size);
}
.num {
  font-variant-numeric: tabular-nums;
  text-align: right;
}
</style>
