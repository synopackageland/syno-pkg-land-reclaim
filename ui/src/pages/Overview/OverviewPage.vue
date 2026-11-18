<script setup lang="ts">
// Overview: volume cards, where the space went, and
// the suggested cleanup; the watch-free sheet (§4.3b). No single "can free"
// total: package data, snapshots and copies are freed in different places
// (seen on a test NAS), so each source is its own suggested row.
import ScanBadge from '@/components/ScanBadge.vue'
import PageStatus from '@/components/PageStatus.vue'
import { reportCompleteness } from '@/components/reportCompleteness'
import { approximationNote } from '@/components/approximation'
import { computed, ref, watch } from 'vue'
import { Button, EmptyState, LoadingArea, Surface, useToast, Space, writeClipboard } from '@synopackageland/ui'
import { Activity, ScanSearch } from '@lucide/vue'
import VolumeCard from '@/components/VolumeCard.vue'
import { VOLUME_TABS_MAX } from '@/components/optionList'
import { changesOnlyInSystemAreas } from '@/components/consistency'
import StateView from '@/components/states/StateView.vue'
import UnsupportedVolumesNote from '@/components/states/UnsupportedVolumesNote.vue'
import SystemAreasNote from '@/components/SystemAreasNote.vue'
import { code, formatBytes, formatPercent, formatRelative, joinParts, locale, t } from '@/i18n'
import { answersVersion } from '@/stores/answers'
import { jobs } from '@/stores/jobs'
import { nav, navigate, openNewScan } from '@/stores/nav'
import { preferences } from '@/stores/preferences'
import { cleanupUnavailableReason } from '@/components/cleanupUnavailable'
import { consistencyTone } from '@/components/consistencyTone'
import { loadVolumes, selectedVolume, volumeList, volumes } from '@/stores/volumes'
import type { Volume } from '@/types'
import ActionList from './ActionList.vue'
import DeepAttribution from './DeepAttribution.vue'
import SpacePanel from './SpacePanel.vue'
import SystemPartitionView from './SystemPartitionView.vue'
import WatchFreeSheet from './WatchFreeSheet.vue'
import { actionItems, breakdown, volumeStatus, volumeUsage, type ActionRow, type VolumeStatus } from './overview'
import { deepCleanable, reportLevel } from './rootfsView'
import { systemPartitionLevel } from './systemPartition'
import { useOverviewData } from './useOverviewData'
import { useRootfsReport } from './useRootfsReport'

const { answers, load } = useOverviewData()
const { report: rootfsReport, loading: rootfsLoading, refreshing: rootfsRefreshing, error: rootfsError, load: loadRootfs, loadResult: loadRootfsResult } = useRootfsReport()
const toast = useToast()
const watchOpen = ref(false)
// The System partition card selects the view in the Overview body (§4 S3).
const systemSelected = ref(false)

watch(() => nav.rootfsOpenRequested, requested => {
  if (!requested) return
  systemSelected.value = true
  if (nav.rootfsResultJobId) { void loadRootfsResult(nav.rootfsResultJobId); nav.rootfsResultJobId = null }
  nav.rootfsOpenRequested = false
}, { immediate: true })

/** Check again (the view's button or New scan): the walk of `/`, then the card's free space. */
async function checkRootfs() {
  await loadRootfs({ force: true })
  await loadVolumes()
}
// A walk in the background swapped in a newer report: the card's free space too.
watch(rootfsRefreshing, (now, before) => {
  if (before && !now && !rootfsError.value) void loadVolumes()
})

watch(() => nav.rootfsCheckRequested, requested => {
  if (!requested) return
  systemSelected.value = true
  nav.rootfsCheckRequested = false
  void checkRootfs()
}, { immediate: true })

const resultOf = (v: Volume) => answers[v.mount.path]?.overview?.results[0] ?? null
const statusOf = (v: Volume) => volumeStatus(v, jobs.list, resultOf(v)?.report)

watch(
  [volumeList, selectedVolume, answersVersion],
  () => {
    // Only the shown volume's report is needed; its cards read the rest from
    // volumes.list (review §1 P0-4).
    const v = selectedVolume.value
    if (v && statusOf(v).kind !== 'no_report') void load(v.mount.path, { why: v.capabilities.reflinks })
  },
  { immediate: true },
)

/** Above four volumes only the shown card is expanded (review §1 P0-4). */
const compactVolumes = computed(() => volumeList.value.length > VOLUME_TABS_MAX)

const volume = computed(() => selectedVolume.value)
const system = computed(() => volumes.document?.system_partition ?? null)
const level = computed(() => systemPartitionLevel(system.value))

// The Full actions row's size and the System partition view come from the same
// `rootfs.report`; a full partition needs
// the number even before the card is selected. Its walk of `/` waits for the
// shown volume's overview: both take the one query slot, and the volume is
// what the page opens on (seen on a test NAS).
const overviewSettled = computed(() => {
  const v = selectedVolume.value
  return !v || !answers[v.mount.path]?.overviewLoading
})
watch(
  [level, systemSelected, overviewSettled],
  () => {
    if (systemSelected.value || (level.value === 'full' && overviewSettled.value)) void loadRootfs()
  },
  { immediate: true },
)

// A check that finds the partition normal again returns to the volume body (§4 S3).
watch(rootfsReport, report => {
  if (systemSelected.value && report && reportLevel(report) === 'ok') {
    systemSelected.value = false
    toast.info(t('rootfsView.backToNormal'))
  }
})

/** The System partition card at the end of the volume row (§4 S2): its level, free space and the way into cleanup. */
const systemCard = computed(() => {
  const partition = system.value
  const state = level.value
  if (!partition || !state || state === 'ok') return null
  const used = partition.total_bytes - partition.available_bytes
  // With the warning off the card stays as the way into cleanup, without its alarm colours.
  const tone = !preferences.systemPartitionWarning ? null : state === 'full' ? ('danger' as const) : ('warning' as const)
  return {
    chip: t(`rootfs.level.${state}`),
    chipTone: tone ?? ('neutral' as const),
    barTone: tone ?? ('primary' as const),
    usedRatio: used / partition.total_bytes,
    usedText: t('overview.volume.used', { used: formatBytes(used), total: formatBytes(partition.total_bytes) }),
    figureLabel: t('rootfs.card.free'),
    figure: formatBytes(partition.available_bytes),
    // The card is the way into cleanup; with the warning off it stays one, without the call to act.
    chipDetails: tone ? t(`rootfs.level.${state}.body`) : undefined,
    action: tone ? t(state === 'full' ? 'rootfs.card.cleanNow' : 'rootfs.card.check') : undefined,
  }
})

function selectVolume(next: Volume) {
  systemSelected.value = false
  volumes.selected = next.mount.path
}

const status = computed(() => (volume.value ? statusOf(volume.value) : null))
// A volume with a scan job never offers to start another one.
const scanActive = computed(() => status.value?.kind === 'scanning' || status.value?.kind === 'paused')
const answer = computed(() => (volume.value ? answers[volume.value.mount.path] : undefined))
// The explanations come from an optional second call: say when they are still
// loading or failed, instead of showing an action list that looks complete.
const whyNote = computed(() => {
  if (answer.value?.whyPending) return t('overview.actions.whyPending')
  if (answer.value?.whyFailed) return t('overview.actions.whyFailed')
  return null
})
const doc = computed(() => answer.value?.overview ?? null)
const result = computed(() => doc.value?.results[0] ?? null)
// report_incompatible: the saved reports are from another format, so there is none to show (SPEC §6.2 F9).
const NO_REPORT_CODES = ['no_report', 'report_incompatible']
const noReport = computed(
  () => status.value?.kind === 'no_report' || !!doc.value?.scan_hints?.length || !!doc.value?.errors.some(e => NO_REPORT_CODES.includes(e.code)),
)
const lastScanSeconds = computed(() => {
  const ms = doc.value?.scan_hints?.[0]?.last_scan?.elapsed_ms
  return ms == null ? null : ms / 1000
})

/** The volume's current reading (volumes.list), not the report's own df. */
const liveUsage = computed(() => (volume.value ? volumeUsage(volume.value) : null))
// df available is lower than df free when the filesystem keeps free space in reserve
// (Btrfs reserve, ext4 root-reserved blocks): say how much can be written.
const writable = computed(() => {
  const df = result.value?.space.df
  return df && df.available_bytes < df.free_bytes ? df.available_bytes : null
})
/** What the space panel adds below its own notes: cleanup here, and the live reading. */
const spaceNotes = computed(() => {
  const v = volume.value
  if (!result.value || !v) return []
  const ext4 = v.filesystem === 'ext4'
  return [
    v.capabilities.cleanup ? '' : joinParts([cleanupUnavailableReason(v), t('state.cleanupUnavailable.how')], locale),
    writable.value != null ? t(ext4 ? 'overview.hero.writableExt4' : 'overview.hero.writable', { bytes: formatBytes(writable.value) }) : '',
    liveUsage.value ? t('overview.hero.usedNow', { used: formatBytes(liveUsage.value.used), total: formatBytes(liveUsage.value.total) }) : '',
  ]
})
/** The Full-only "Things you can do" row. Its size
 * is `rootfs.report`'s movable bytes (MR C2), unknown until the report loads;
 * "up to" deep mode's total when that moves more than standard.
 * Off with the warning; the System partition card stays for cleaning up by hand. */
const systemRow = computed<ActionRow | null>(() => {
  if (level.value !== 'full' || !preferences.systemPartitionWarning) return null
  const report = rootfsReport.value
  const standard = report?.cleanable.items ? report.cleanable.bytes : null
  const deep = report ? deepCleanable(report) : null
  const row = { key: 'system_partition', category: 'system_partition', action: 'system_partition', inBreakdown: false, shares: [] } as const
  if (deep && deep.bytes > (standard ?? 0)) return { ...row, shares: [], bound: 'up_to', bytes: deep.bytes }
  return { ...row, shares: [], bound: 'exact', bytes: standard }
})
/** One breakdown for the chart, its legend and the suggested cleanup. */
const items = computed(() => (result.value ? breakdown(result.value, answer.value?.why?.results[0]?.reasons ?? null) : []))
const rows = computed<ActionRow[]>(() => {
  const base = actionItems(items.value)
  return systemRow.value ? [systemRow.value, ...base] : base
})
const completeness = computed(() => reportCompleteness(result.value?.report, doc.value?.completion, !!doc.value?.errors.length))
// Compact coverage must not absorb interrupted answers or actual read errors.
const failedRead = computed(() => !!doc.value && (!['complete', 'partial'].includes(doc.value.completion) || !!doc.value.errors.length))
const unknownReasons = computed(() => result.value?.parts.find(p => p.kind === 'unknown')?.reasons ?? [])
const unknownReasonTexts = computed(() => unknownReasons.value.map(r => `${code('unknown_reason', r.code)}: ${formatBytes(r.bytes)}`))
const errorCodes = computed(() => {
  const apiError = answer.value?.error
  return apiError ? [{ set: 'api', value: apiError.code }] : (doc.value?.errors ?? []).map(e => ({ set: 'error', value: e.code }))
})
// A changed-during-scan report whose changes are all outside the volume's
// shared-folder subvolumes: DSM's own writes, which do not block cleanup there.
const systemAreasOnly = computed(
  () => !!result.value && changesOnlyInSystemAreas(result.value.report, volume.value?.shares ?? []),
)
const changeText = computed(() =>
  [
    ...(systemAreasOnly.value ? [t('common.consistency.systemAreasOnly')] : []),
    t('overview.chip.changedTitle'),
    ...(result.value?.report.change_reasons ?? []).map(r => code('change', r.kind)),
  ].join('\n'),
)

// Coverage belongs to the existing report status, not a separate block.
// Missing legacy evidence still says partial; presentation never certifies it.
const reportDetails = computed(() => {
  const c = completeness.value
  if (c.warn) return [t('state.partial.title'), t('state.partial.body'), ...unknownReasonTexts.value].join('\n')
  if (result.value?.report.attribution?.approximation) return [approximationNote(result.value.report), ...unknownReasonTexts.value].join('\n')
  if (c.unknownNote) return [t('state.unattributed.note', { bytes: formatBytes(c.unknownBytes) }), ...unknownReasonTexts.value].join('\n')
  return ''
})
const statusDetails = computed(() => reportDetails.value || (result.value?.report.consistency === 'changed-during-scan' ? changeText.value : ''))

type Tone = 'neutral' | 'info' | 'success' | 'warning' | 'danger'

function chip(s: VolumeStatus): { text: string; tone: Tone } {
  if (s.kind === 'no_report') return { text: t('overview.chip.notScanned'), tone: 'neutral' }
  if (s.kind === 'scanned')
    return {
      text: t('overview.chip.scanned', { time: formatRelative(s.at), consistency: s.consistency ? code('consistency', s.consistency) : t('common.unknown') }),
      tone: consistencyTone(s.consistency),
    }
  // Without a share of the last scan to compare against there is no percent to show.
  return { text: s.percent == null ? t(`overview.chip.${s.kind}.plain`) : t(`overview.chip.${s.kind}`, { percent: formatPercent(s.percent) }), tone: 'info' }
}

function card(v: Volume) {
  const r = resultOf(v)
  const c = chip(statusOf(v))
  const { used, total } = volumeUsage(v, r)
  return {
    chip: c.text,
    chipTone: c.tone,
    chipDetails: v === volume.value ? statusDetails.value : undefined,
    usedRatio: used != null && total ? used / total : 0,
    usedText: t('overview.volume.usedScanned', { used: formatBytes(used), total: formatBytes(total) }),
    // Free space, like the System partition card; what to clean is below, by source.
    figureLabel: t('overview.volume.free'),
    figure: formatBytes(used != null && total != null ? Math.max(0, total - used) : null),
  }
}

const reload = () => volume.value && load(volume.value.mount.path, { why: volume.value.capabilities.reflinks, force: true })

function copyDetails() {
  const e = answer.value?.error
  void writeClipboard(JSON.stringify({ code: e?.code, exit: e?.exit, detail: e?.detail, volume: volume.value?.mount.path }))
}
</script>

<template>
  <div v-if="volume" class="overview">
    <UnsupportedVolumesNote />
    <Space wrap :spacing="12" align-items="stretch" class="volume-row">
      <VolumeCard
        v-for="v in volumeList"
        :key="v.mount.path"
        v-bind="card(v)"
        :name="v.mount.path.replace(/^\//, '')"
        :selected="v === volume && !systemSelected"
        :compact="compactVolumes && v !== volume"
        @select="selectVolume(v)"
      />
      <VolumeCard v-if="systemCard" v-bind="systemCard" :name="t('rootfs.name')" :selected="systemSelected" @select="systemSelected = true" />
    </Space>

    <Space v-if="status?.kind === 'paused'" wrap :spacing="12" align-items="stretch" class="paused-actions">
      <Button variant="primary" size="sm" @click="navigate('scanning')">{{ t('overview.paused.view') }}</Button>
    </Space>

    <template v-if="result">
      <ScanBadge v-if="status?.kind === 'scanned'" :report="result.report" :consistency="status.consistency ?? undefined" :details="statusDetails" />
      <PageStatus v-else :label="chip(status!).text" :tone="chip(status!).tone" />
    </template>

    <SystemPartitionView
      v-if="systemSelected"
      :report="rootfsReport"
      :loading="rootfsLoading"
      :refreshing="rootfsRefreshing"
      :error="rootfsError"
      @refresh="checkRootfs"
    />
    <template v-else>
      <StateView v-if="noReport && !volume.reports" kind="no_report" :last-scan-seconds="lastScanSeconds">
        <Button variant="primary" @click="openNewScan">{{ t('state.noReport.scan') }}</Button>
      </StateView>
      <EmptyState v-else-if="noReport" :title="t('overview.needsVolumeScan.title')" :description="t('overview.needsVolumeScan.body')" :icon="ScanSearch">
        <template #actions>
          <Button v-if="scanActive" variant="primary" @click="navigate('scanning')">{{ t('newScan.busy.view') }}</Button>
          <Button v-else variant="primary" @click="openNewScan">{{ t('state.noReport.scan') }}</Button>
          <Button variant="ghost" @click="navigate('explore')">{{ t('overview.needsVolumeScan.explore') }}</Button>
        </template>
      </EmptyState>
      <StateView v-else-if="answer?.error?.code === 'busy'" kind="busy">
        <Button size="sm" @click="reload">{{ t('common.retry') }}</Button>
      </StateView>
      <StateView v-else-if="answer?.error || doc?.completion === 'failed'" kind="error" :codes="errorCodes">
        <Button size="sm" @click="reload">{{ t('common.retry') }}</Button>
        <Button size="sm" variant="ghost" @click="copyDetails">{{ t('common.copyDetails') }}</Button>
      </StateView>
      <LoadingArea v-else-if="!result && answer?.overviewLoading !== false" :text="t('common.loading')" class="loading" />
      <StateView v-else-if="!result" kind="error" :codes="errorCodes.length ? errorCodes : [{ set: 'api', value: 'cli_error' }]">
        <Button size="sm" @click="reload">{{ t('common.retry') }}</Button>
      </StateView>

      <template v-else>
        <StateView v-if="failedRead" kind="partial" :codes="errorCodes" />
        <SystemAreasNote :report="result.report" :shares="volume.shares" />
        <DeepAttribution :result="result" :mount="volume.mount" />

        <SpacePanel :result="result" :items="items" :lead="volume.filesystem === 'btrfs' ? t('overview.donut.btrfsNote') : ''" :extra-notes="spaceNotes" />

        <Surface class="actions-panel">
          <Space wrap :spacing="12" align-items="baseline" class="actions-head">
            <h2>{{ t('overview.actions.title') }}</h2>
            <span v-if="whyNote" class="why-note">{{ whyNote }}</span>
            <Button v-if="volume.capabilities.reflinks" size="sm" class="watch-button" @click="watchOpen = true"><Activity class="size-4" />{{ t('overview.actions.watch') }}</Button>
          </Space>
          <ActionList :rows="rows" @watch="watchOpen = true" @system-partition="systemSelected = true" />
        </Surface>
      </template>
    </template>

    <WatchFreeSheet v-if="volume.capabilities.reflinks" v-model:open="watchOpen" :volume="volume.mount.path" @refresh="reload" />
  </div>
</template>

<style scoped>
.overview {
  display: flex;
  flex-direction: column;
  gap: var(--ui-section-gap);
  container-type: inline-size;
}

.actions-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.actions-head h2 {
  font-size: var(--ui-font-size-lg);
  font-weight: 600;
}
.watch-button {
  margin-left: auto;
}
.loading {
  height: 240px;
}
.why-note {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
</style>
