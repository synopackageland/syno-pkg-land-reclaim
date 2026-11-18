<script setup lang="ts">
// Watch free space side sheet: samples while open.
import { computed, onUnmounted, watch } from 'vue'
import { Button, Sheet, LoadingArea } from '@synopackageland/ui'
import LineChart from '@/components/LineChart.vue'
import StateView from '@/components/states/StateView.vue'
import { formatBytes, formatCount, joinSentences, locale, t } from '@/i18n'
import { useWatchFree } from './useWatchFree'

const props = defineProps<{ open: boolean; volume: string }>()
const emit = defineEmits<{ 'update:open': [value: boolean]; refresh: [] }>()

const sampler = useWatchFree(() => props.volume)
const { points, running, error, delta, pending, used } = sampler

watch(
  () => props.open,
  open => {
    sampler.reset()
    if (open) void sampler.start()
  },
  { immediate: true },
)
onUnmounted(sampler.stop)

const chartPoints = computed(() => points.value.flatMap(p => (p.free == null ? [] : [{ x: p.x, y: p.free }])))
const xMax = computed(() => Math.max(sampler.windowMs, points.value.at(-1)?.x ?? 0))
const seconds = computed(() => Math.round(xMax.value / 1000))
const xLabels = computed(() => [0, seconds.value / 2, seconds.value].map(n => t('overview.watch.seconds', { count: Math.round(n) })))
const signed = (bytes: number | null) => (bytes == null ? formatBytes(null) : `${bytes >= 0 ? '+' : '−'}${formatBytes(Math.abs(bytes))}`)
const change = (pair: { first: number | null; last: number | null } | null, format: (n: number | null) => string) =>
  pair ? t('overview.watch.change', { first: format(pair.first), last: format(pair.last) }) : format(null)
const cleanupDone = computed(() => !running.value && (pending.value?.first ?? 0) > 0 && pending.value?.last === 0)

function refresh() {
  emit('refresh')
  emit('update:open', false)
}
</script>

<template>
  <Sheet :aria-describedby="undefined" :open="open" :title="t('overview.watch.title')" :close-label="t('common.close')" size="440px" @update:open="emit('update:open', $event)">
    <div class="watch">
      <p class="muted">{{ t('overview.watch.intro', { volume }) }}</p>
      <div class="watch-delta">
        <span>{{ t('overview.watch.chart', { seconds }) }}</span>
        <strong class="num">{{ signed(delta) }}</strong>
      </div>
      <LineChart :points="chartPoints" :x-max="xMax" :x-labels="xLabels" :title="t('overview.watch.chart', { seconds })" :width="400" />
      <dl class="watch-stats">
        <dt>{{ t('overview.watch.pending') }}</dt>
        <dd class="num">{{ change(pending, formatCount) }}</dd>
        <dt>{{ t('overview.watch.used') }}</dt>
        <dd class="num">{{ change(used, formatBytes) }}</dd>
      </dl>
      <StateView v-if="error" kind="error" :error="error" />
      <p class="muted">
        {{ joinSentences([...(cleanupDone ? [t('overview.watch.done')] : []), t('overview.watch.note')], locale) }}
      </p>
      <LoadingArea v-if="running" :text="t('overview.watch.measuring')" />
    </div>
    <template #footer>
      <Button :disabled="running" @click="sampler.start()">{{ t('overview.watch.more') }}</Button>
      <Button variant="primary" @click="refresh">{{ t('overview.watch.refresh') }}</Button>
    </template>
  </Sheet>
</template>

<style scoped>
.watch {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.watch-delta {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
}
.watch-delta strong {
  font-size: 20px;
  color: var(--ui-success);
}
.watch-stats {
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 6px 12px;
  font-size: var(--ui-font-size);
}
.num {
  font-variant-numeric: tabular-nums;
}
.muted {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-muted);
}
</style>
