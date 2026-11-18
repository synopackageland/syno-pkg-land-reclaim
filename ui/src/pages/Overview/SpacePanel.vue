<script setup lang="ts">
// "Where the space went": donut by type (the page's breakdown, which the
// suggested cleanup lists too), by shared folder or by package with its
// legend, amounts and shares; the Storage Manager reconciliation and the
// unknown cause are notes. `lead` stays in view: on
// Btrfs, what deleting frees depends on shared data.
import { computed, ref } from 'vue'
import { Button, Surface, Tabs, Tooltip } from '@synopackageland/ui'
import ScanNotes from '@/components/ScanNotes.vue'
import Donut, { type DonutSegment } from '@/components/Donut.vue'
import { shareLabel } from '@/components/charts'
import { code, formatBytes, formatPercent, joinSentences, locale, t } from '@/i18n'
import { navigate } from '@/stores/nav'
import { byFolder, byPackage, chartItems, type BreakdownItem, type OverviewResult, type Segment } from './overview'
import { amountOf, colorOf, labelOf } from './breakdownView'

const props = defineProps<{ result: OverviewResult; items: BreakdownItem[]; lead?: string; extraNotes?: string[] }>()

// The shared folder and package tabs use a categorical palette of the same tokens.
const FOLDER_COLORS = ['var(--ui-blue-9)', 'var(--ui-blue-12)', 'var(--ui-green-9)', 'var(--ui-orange-9)', 'var(--ui-blue-7)', 'var(--ui-orange-11)', 'var(--ui-red-9)']
const OTHER_COLOR = 'var(--ui-gray-10)'

const mode = ref('type')
const modes = computed(() => [
  { value: 'type', label: t('overview.donut.byType') },
  { value: 'folder', label: t('overview.donut.byFolder') },
  { value: 'packages', label: t('overview.donut.byPackage') },
])

const snapshotsNotMaterialized = computed(() => props.result.report.coverage.snapshots_not_materialized ?? 0)

const df = computed(() => props.result.space.df)

function segmentLabel(s: Segment) {
  if (mode.value === 'packages') return s.service ? packageName(s.service, s.label ?? s.service) : t('overview.donut.otherPackages')
  return s.label ?? t('overview.donut.other')
}

/** By type: the page's breakdown, as the suggested cleanup lists it. */
const typeSegments = computed(() =>
  chartItems(props.items).map(item => ({ key: item.key, value: item.bytes, label: labelOf(item), color: colorOf(item)!, amount: amountOf(item.bytes, item.bound) })),
)
const otherSegments = computed(() =>
  (mode.value === 'packages' ? byPackage(props.result) : byFolder(props.result)).map((s, i) => ({
    key: s.key,
    value: s.value,
    label: segmentLabel(s),
    color: s.key === 'other' ? OTHER_COLOR : FOLDER_COLORS[i % FOLDER_COLORS.length],
    amount: formatBytes(s.value),
  })),
)

const segments = computed<DonutSegment[]>(() => {
  const parts = mode.value === 'type' ? typeSegments.value : otherSegments.value
  const counted = parts.filter(s => s.key !== 'free').reduce((sum, s) => sum + s.value, 0)
  const used = df.value?.used_bytes ?? counted
  const total = df.value?.total_bytes ?? 0
  return parts.map(s => {
    // Free space is a share of the volume; every other part a share of the used space.
    const ofUsed = s.key === 'free' ? undefined : shareLabel(s.value, used, formatPercent)
    const ofVolume = shareLabel(s.value, total, formatPercent)
    return {
      ...s,
      share: ofUsed ?? ofVolume,
      details: [
        ofUsed && t('overview.donut.ofUsed', { percent: ofUsed }),
        ofVolume && t('overview.donut.ofVolume', { percent: ofVolume }),
      ].filter((line): line is string => Boolean(line)),
    }
  })
})

const recon = computed(() => props.result.reconciliation.data)
const reconText = computed(() =>
  t(`overview.recon.${recon.value.status}`, {
    sum: formatBytes(recon.value.parts_sum_bytes),
    residual: formatBytes(recon.value.residual_bytes == null ? null : Math.abs(recon.value.residual_bytes)),
  }),
)
const reconDetail = computed(() =>
  recon.value.status === 'inode_allocations'
    ? ''
    : t('overview.recon.detail', { used: formatBytes(recon.value.space_info_used_bytes), tolerance: formatBytes(recon.value.tolerance_bytes) }),
)
function viewAll() {
  navigate('packages')
}
const packageName = (service: string, fallback: string) => {
  const key = `code.service.${service}`
  return t(key) === key ? fallback : t(key)
}
const unknownPart = computed(() => props.result.parts.find(p => p.kind === 'unknown'))
const unknownReasons = computed(() => (unknownPart.value?.reasons ?? []).map(r => `${code('unknown_reason', r.code)}: ${formatBytes(r.bytes)}`).join('\n'))
const notes = computed(() => [
  joinSentences([reconText.value, reconDetail.value], locale),
  unknownPart.value?.bytes ? [t('overview.recon.unknownBytes', { bytes: formatBytes(unknownPart.value.bytes) }), unknownReasons.value].filter(Boolean).join('\n') : '',
  snapshotsNotMaterialized.value ? t('overview.donut.snapshot_coverage', { count: snapshotsNotMaterialized.value }) : '',
  ...(props.extraNotes ?? []),
])
</script>

<template>
  <Surface class="space-panel">
    <div class="space-head">
      <h2>{{ t('overview.donut.title') }}</h2>
      <Tabs v-model="mode" :items="modes" />
    </div>
    <p v-if="lead" class="lead" data-testid="btrfs-note">{{ lead }}</p>
    <p v-if="!segments.length" class="empty">{{ t(mode === 'packages' ? 'packages.empty.title' : 'overview.donut.empty') }}</p>
    <Donut v-else :segments="segments" :title="t('overview.donut.title')">
      <strong class="num">{{ formatBytes(df?.used_bytes) }}</strong>
      <span class="used-of">{{ t('overview.donut.used') }}</span>
      <span class="used-of">{{ t('overview.donut.ofTotal', { total: formatBytes(df?.total_bytes) }) }}</span>
      <template #label="{ segment }">
        <Tooltip v-if="segment.key === 'small_unknown'" :content="unknownReasons"><span>{{ segment.label }}</span></Tooltip>
        <template v-else>{{ segment.label }}</template>
      </template>
    </Donut>
    <Button v-if="mode === 'packages'" variant="ghost" size="sm" class="view-all" @click="viewAll">{{ t('overview.packages.viewAll') }}</Button>
    <ScanNotes :notes="notes" />
  </Surface>
</template>

<style scoped>
.space-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.space-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
h2 {
  font-size: var(--ui-font-size-lg);
  font-weight: 600;
}
.view-all {
  align-self: flex-start;
}
.num {
  font-variant-numeric: tabular-nums;
}
.lead {
  margin: 0;
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.empty,
.used-of {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
</style>
