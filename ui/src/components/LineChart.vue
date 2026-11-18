<script setup lang="ts">
// Small SVG line with the area under it: x runs from 0
// to xMax, y is fitted to the data (at least minYSpan wide), the last point marked when it has a
// label. Watch free space and History use it.
import { computed } from 'vue'
import { linePaths, type Point } from './charts'

const props = withDefaults(
  defineProps<{
    points: Point[]
    xMax: number
    title: string
    /** Labels spread evenly under the x axis, e.g. 0 s, 30 s, 60 s. */
    xLabels?: string[]
    /** The last point's value, drawn next to a dot on it. */
    lastLabel?: string
    width?: number
    height?: number
    color?: string
    /** The y range is at least this wide (charts.ts linePaths). */
    minYSpan?: number
  }>(),
  { xLabels: () => [], width: 388, height: 150, color: 'var(--ui-success)', minYSpan: 0 },
)

const paths = computed(() => linePaths(props.points, props))
const grid = computed(() => [0, 0.5, 1].map(f => Math.round(f * (props.height - 1)) + 0.5))
</script>

<template>
  <figure class="line-chart" :style="{ width: `${width}px` }">
    <svg :width="width" :height="height" :viewBox="`0 0 ${width} ${height}`" role="img">
      <title>{{ title }}</title>
      <path v-for="y in grid" :key="y" :d="`M0 ${y} H${width}`" stroke="var(--ui-border-subtle)" />
      <path :d="paths.area" :fill="color" fill-opacity="0.12" />
      <path :d="paths.line" fill="none" :stroke="color" stroke-width="2.5" stroke-linejoin="round" />
      <template v-if="lastLabel && paths.end">
        <circle :cx="paths.end.x" :cy="paths.end.y" r="4" :fill="color" />
        <text :x="Math.min(paths.end.x, width - 6)" :y="Math.max(paths.end.y - 10, 12)" text-anchor="end" class="line-chart-value">
          {{ lastLabel }}
        </text>
      </template>
    </svg>
    <figcaption v-if="xLabels.length" class="line-chart-axis">
      <span v-for="(label, index) in xLabels" :key="index">{{ label }}</span>
    </figcaption>
  </figure>
</template>

<style scoped>
.line-chart {
  margin: 0;
  max-width: 100%;
}
/* Shrinks with a narrower container, keeping its proportions. */
.line-chart svg {
  display: block;
  max-width: 100%;
  height: auto;
  overflow: visible;
}
.line-chart-value {
  fill: var(--ui-text);
  font-size: 12px;
}
.line-chart-axis {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  color: var(--ui-text-muted);
}
</style>
