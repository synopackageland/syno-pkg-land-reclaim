<script setup lang="ts">
// History (掃描紀錄): the used-space trend of one
// volume, one row per scan, and the saved results with the cache controls.
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, onMounted, ref, watch } from 'vue'
import {
  Badge,
  Button,
  Callout,
  CellStatus,
  CellText,
  Dialog,
  EmptyState,
  LoadingArea,
  Space,
  Surface,
  useToast,
  type DataGridColumn,
} from '@synopackageland/ui'
import { PageActions } from '@synopackageland/sdk/vue'
import { History as HistoryIcon } from '@lucide/vue'
import { api, ApiError } from '@/api'
import { code, formatBytes, formatCount, formatDateTime, formatDuration, formatRelative, formatVolume, joinParts, locale, t } from '@/i18n'
import { authorizeRootWork } from '@/stores/authorization'
import { jobs, pollNow, runningJobs } from '@/stores/jobs'
import { navigate, openNewScan, openRootfsView } from '@/stores/nav'
import { selectedVolume, volumeList, volumes } from '@/stores/volumes'
import ReviewDialog from '@/pages/Review/ReviewDialog.vue'
import RootfsApplyStep from '@/pages/RootfsApply/RootfsApplyStep.vue'
import LineChart from '@/components/LineChart.vue'
import VolumePicker from '@/components/VolumePicker.vue'
import StateView from '@/components/states/StateView.vue'
import RootfsHistorySection from '@/pages/RootfsApply/RootfsHistorySection.vue'
import { systemPartitionLevel } from '@/pages/Overview/systemPartition'
import type { RootfsHistoryRecord } from '@/pages/RootfsApply/rootfsTypes'
import { coversVolume, historyRows, visibleOutcome, isKnownOutcome, outcomeTone, trendAxisLabels, trendPoints, trendSubtitle as subtitleFor, type ScanRow } from './history'
import { scopeLabel } from '@/components/scanTarget'
import type { CacheDocument, HistoryDocument, Job, ScanDocument } from '@/types'

const history = ref<HistoryDocument | null>(null)
const cache = ref<CacheDocument | null>(null)
const loading = ref(true)
const error = ref<ApiError | null>(null)
const toast = useToast()
const recoveryJob = ref<Job | null>(null)
const recoveryOpen = ref(false)
const recoveryPaths = computed(() => (recoveryJob.value?.targets ?? []).map(path => ({ ...path, reclaimable: null })))
const clearOpen = ref(false)
const clearing = ref(false)
const resultOpen = ref(false)
const resultId = ref<string | null>(null)
const selectedResult = ref<ScanDocument | null>(null)
const resultLoading = ref(false)
const resultError = ref<ApiError | null>(null)
const whyRow = ref<ScanRow | null>(null)
const rootfsApplies = ref<RootfsHistoryRecord[]>([])
const rootfsHistoryLoading = ref(true)
const rootfsHistoryError = ref(false)
/** The volume the trend shows; null follows the globally selected volume. */
const trendPick = ref<string | null>(null)

const mount = computed(() => trendPick.value ?? selectedVolume.value?.mount.path ?? null)
const mountName = computed(() => (mount.value ? formatVolume(mount.value) : ''))
const volumeChoices = computed(() => volumeList.value.map(volume => ({ value: volume.mount.path, label: formatVolume(volume.mount.path) })))

const rows = computed<ScanRow[]>(() => (history.value ? historyRows(history.value, jobs.list) : []))
const tableRows = computed<Record<string, unknown>[]>(() => rows.value)
const trend = computed(() => (history.value && mount.value ? trendPoints(history.value, mount.value) : []))
/** x: milliseconds since the first scan, so the points sit at their scan time. */
const chartPoints = computed(() => {
  const first = trend.value[0] ? Date.parse(trend.value[0].at) : 0
  return trend.value.map(point => ({ x: Date.parse(point.at) - first, y: point.value }))
})
const chartSpan = computed(() => chartPoints.value.at(-1)?.x ?? 0)
/** Up to five labels, evenly spread from the first scan to the last. */
const chartLabels = computed(() => {
  const first = trend.value[0] ? Date.parse(trend.value[0].at) : 0
  return trendAxisLabels(first, chartSpan.value, Math.min(5, chartPoints.value.length), { locale })
})
/** A tenth of the used space: rescans a few MB apart draw flat, not as a ramp. */
const chartMinSpan = computed(() => Math.max(0, ...trend.value.map(point => point.value)) / 10)
const newest = computed(() => rows.value[0] ?? null)
const rootfsCanCleanUp = computed(() => {
  const level = systemPartitionLevel(volumes.document?.system_partition)
  return level === 'getting_full' || level === 'full'
})

const columns = computed<DataGridColumn[]>(() => [
  { key: 'started', type: 'date', sortable: true, label: t('history.table.time'), minWidth: '190px' },
  { key: 'scope', sortable: true, compare: (_a, _b, a, b) => scopeText(asScan(a)).localeCompare(scopeText(asScan(b))), label: t('history.table.scope'), minWidth: '180px' },
  { key: 'profile', sortable: true, label: t('history.table.mode'), width: '86px', priority: 2 },
  { key: 'elapsedMs', type: 'number', sortable: true, label: t('history.table.duration'), width: '90px', priority: 3 },
  { key: 'entries', type: 'number', sortable: true, label: t('history.table.entries'), width: '110px', priority: 1 },
  { key: 'peakRssBytes', type: 'number', sortable: true, label: t('history.table.memory'), width: '90px', priority: 0 },
  { key: 'outcome', sortable: true, label: t('history.table.outcome'), width: '150px' },
  { key: 'actions', type: 'actions', label: t('history.table.actions'), width: '210px' },
])

function openRecovery(row: ScanRow) {
  recoveryJob.value = jobs.list.find(job => job.job_id === row.id) ?? null
  recoveryOpen.value = recoveryJob.value != null
}

function scopeText(row: ScanRow) {
  if (row.kind === 'rootfs-apply') return t('common.job.kind.rootfs-apply')
  const scope = scopeLabel(row.targets, row.volume)
  return row.kind === 'cleanup-apply' ? joinParts([t('common.job.kind.cleanup-apply'), scope], locale) : scope
}

function asScan(row: Record<string, unknown>): ScanRow {
  return row as unknown as ScanRow
}

function outcomeLabel(outcome: string) {
  if (outcome === 'approximate') return code('consistency', outcome)
  if (outcome === 'small_unknown') return t('code.consistency.small_unknown')
  if (outcome === 'changed-during-scan') return code('consistency', outcome)
  return isKnownOutcome(outcome) ? t(`history.outcome.${outcome}`) : t('code.unknown', { code: outcome })
}

function modeLabel(profile: string | null) {
  return profile === 'fast' || profile === 'normal' || profile === 'low' ? t(`history.mode.${profile}`) : t('common.unknown')
}

const chip = computed(() =>
  newest.value ? t('history.chip', { time: formatRelative(newest.value.finished ?? newest.value.started), outcome: outcomeLabel(visibleOutcome(newest.value)) }) : null,
)

const trendSubtitle = computed(() => {
  if (trend.value.length < 2) return null
  const first = trend.value[0]
  const last = trend.value.at(-1)
  if (!last) return null
  return subtitleFor(formatBytes(first.value), formatBytes(last.value), Date.parse(last.at) - Date.parse(first.at), t)
})

const partial = computed(() => {
  if (!mount.value) return false
  const row = rows.value.find(row => coversVolume(row, mount.value!))
  return !!row && visibleOutcome(row) === 'partial'
})

const reportCount = computed(() => volumeList.value.find(volume => volume.mount.path === mount.value)?.reports ?? 0)
const cacheFs = computed(() => cache.value?.filesystems.find(fs => fs.mount.path === mount.value) ?? null)
const cacheUnits = computed(() => cacheFs.value?.units ?? [])
const reusableUnits = computed(() => cacheUnits.value.filter(unit => unit.reusable).length)
const cacheSummary = computed(() => cacheFs.value
  ? t('history.results.cache.value', { size: formatBytes(cacheFs.value.bytes), total: formatCount(cacheUnits.value.length) })
  : t('history.results.none'))
const clearBlocked = computed(() => {
  const at = mount.value
  return at != null && runningJobs.value.some(job => jobTouches(job, at))
})

function jobTouches(job: Job, at: string) {
  return job.filesystems.some(fs => fs.mount.path === at) || job.targets.some(target => coversVolume({ targets: [target] }, at))
}

const whyOpen = computed({
  get: () => whyRow.value != null,
  set: (open: boolean) => {
    if (!open) whyRow.value = null
  },
})

const whyText = computed(() => {
  const outcome = whyRow.value?.outcome
  if (outcome === 'superseded') return t('history.why.superseded')
  if (outcome === 'failed') return t('history.why.failed')
  return t('history.why.partial')
})

async function loadHistory() {
  loading.value = true
  error.value = null
  try {
    history.value = await api('history.list', {})
  } catch (cause) {
    error.value = cause instanceof ApiError ? cause : new ApiError('unknown')
  } finally {
    loading.value = false
  }
}

async function loadRootfsHistory() {
  rootfsHistoryLoading.value = true
  rootfsHistoryError.value = false
  try {
    rootfsApplies.value = (await api('rootfs.history', {})).applies
  } catch {
    rootfsHistoryError.value = true
  } finally {
    rootfsHistoryLoading.value = false
  }
}

async function loadCache() {
  const at = mount.value
  if (!at) return
  try {
    cache.value = await api('cache.status', { volume: at })
  } catch {
    cache.value = null
  }
}

async function clearCache() {
  const at = mount.value
  if (!at) return
  clearing.value = true
  try {
    if (!(await authorizeRootWork())) return
    const result = await api('cache.clear', { volume: at })
    cache.value = result
    const cleared = result.filesystems.reduce((sum, fs) => sum + (fs.cleared ?? 0), 0)
    toast.success(t('history.results.cleared', { count: formatCount(cleared) }))
    clearOpen.value = false
  } catch (cause) {
    const busy = cause instanceof ApiError && cause.code === 'busy'
    toast.warning(t(busy ? 'history.results.busy' : 'history.results.failed'))
  } finally {
    clearing.value = false
  }
}

async function openResult(row: ScanRow) {
  const id = row.id
  resultId.value = id
  selectedResult.value = null
  resultError.value = null
  resultLoading.value = true
  resultOpen.value = true
  try {
    const document = await api('job.result', { job_id: id })
    if (resultId.value !== id) return
    if (document.job_id !== id || document.command !== 'scan' || !('filesystems' in document)) throw new ApiError('unknown')
    selectedResult.value = document as ScanDocument
  } catch (error) {
    if (resultId.value === id) resultError.value = error instanceof ApiError ? error : new ApiError('unknown')
  } finally {
    if (resultId.value === id) resultLoading.value = false
  }
}

onMounted(() => {
  void loadHistory()
  void loadRootfsHistory()
  void loadCache()
  pollNow()
})

function checkAndCleanUp() {
  openRootfsView()
}

watch(mount, () => void loadCache())
</script>

<template>
  <div class="history-page">
    <PageActions>
      <Badge v-if="chip">{{ chip }}</Badge>
    </PageActions>
    <StateView v-if="error" kind="error" :error="error">
      <Button @click="loadHistory">{{ t('common.retry') }}</Button>
    </StateView>
    <LoadingArea v-else-if="loading || (rootfsHistoryLoading && rows.length === 0)" :text="t('common.loading')" />
    <EmptyState
      v-else-if="rows.length === 0 && !rootfsApplies.length && !rootfsHistoryError"
      :title="t('history.empty.title')"
      :description="t('history.empty.body')"
      :icon="HistoryIcon"
    >
      <template #actions>
        <Button variant="primary" @click="openNewScan">{{ t('history.empty.scan') }}</Button>
      </template>
    </EmptyState>

    <template v-else>
      <Callout v-if="rootfsHistoryError" :dismissible="false" tone="warning" :title="t('rootfsApply.history.loadError')">
        <template #actions>
          <Button size="sm" @click="loadRootfsHistory">{{ t('common.retry') }}</Button>
        </template>
      </Callout>

      <template v-if="rows.length">
        <div class="history-cards">
          <Surface>
            <div class="card-head">
              <div>
                <h2>{{ t('history.trend.title', { volume: mountName }) }}</h2>
                <p v-if="trendSubtitle" class="muted">{{ trendSubtitle }}</p>
                <p v-else-if="trend.length === 1" class="muted">
                  {{ t('history.trend.onlyOne', { value: formatBytes(trend[0].value) }) }}
                </p>
              </div>
              <VolumePicker
                v-if="volumeChoices.length > 1"
                class="volume-pick"
                v-bind="{ ariaLabel: t('history.trend.volume') }"
                :model-value="mount ?? ''"
                :volumes="volumeChoices"
                @update:model-value="trendPick = $event"
              />
            </div>
            <LineChart
              v-if="chartPoints.length > 1"
              :points="chartPoints"
              :x-max="chartSpan"
              :x-labels="chartLabels"
              :min-y-span="chartMinSpan"
              :last-label="formatBytes(trend.at(-1)?.value)"
              :title="t('history.trend.chart', { volume: mountName })"
              :width="700"
              color="var(--ui-primary)"
            />
            <p v-else class="muted">{{ t(trend.length === 1 ? 'history.trend.needTwo' : 'history.trend.noData') }}</p>
          </Surface>

          <Surface class="results-card">
            <h2 class="results-title">{{ t('history.results.title') }}</h2>
            <p class="muted">{{ mountName }}</p>
            <dl class="results">
              <div><dt>{{ t('history.results.reports') }}</dt><dd>{{ reportCount ? formatCount(reportCount) : t('history.results.none') }}</dd></div>
              <div><dt>{{ t('history.results.cache') }}</dt><dd>{{ cacheSummary }}</dd></div>
              <div v-if="cacheUnits.length">
                <dt>{{ t('history.results.units') }}</dt>
                <dd>{{ t('history.results.units.value', { total: formatCount(cacheUnits.length), reusable: formatCount(reusableUnits) }) }}</dd>
              </div>
            </dl>
            <p class="muted">{{ t(clearBlocked ? 'history.results.clearBlocked' : 'history.results.hint') }}</p>
            <Button :disabled="clearBlocked" @click="clearOpen = true">{{ t('history.results.clear') }}</Button>
          </Surface>
        </div>

        <Callout v-if="partial" tone="warning" :title="t('history.partial.title')">{{ t('history.partial.body') }}</Callout>

        <Surface padding="none" class="history-table">
          <ReclaimGrid
            table-id="history"
            class="history-grid"
            :columns="columns"
            :rows="tableRows"
            row-key="id"
          >
            <template #cell-started="{ row }"><CellText :text="formatDateTime(asScan(row).started)" /></template>
            <template #cell-scope="{ row }"><CellText :text="scopeText(asScan(row))" wrap /></template>
            <template #cell-profile="{ row }"><CellText :text="modeLabel(asScan(row).profile)" /></template>
            <template #cell-elapsedMs="{ row }"><CellText class="tabular-nums" :text="formatDuration(asScan(row).elapsedMs == null ? null : asScan(row).elapsedMs! / 1000)" /></template>
            <template #cell-entries="{ row }"><CellText class="tabular-nums" :text="formatCount(asScan(row).entries)" /></template>
            <template #cell-peakRssBytes="{ row }"><CellText class="tabular-nums" :text="formatBytes(asScan(row).peakRssBytes)" /></template>
            <template #cell-outcome="{ row }">
              <CellStatus
                :tone="outcomeTone(visibleOutcome(asScan(row)))"
                :label="asScan(row).reason === 'not_started' ? t('common.job.notStarted') : outcomeLabel(visibleOutcome(asScan(row)))"
                :detail="asScan(row).missingResult && asScan(row).reason !== 'not_started' ? t('common.job.interrupted') : undefined"
              />
            </template>
            <template #cell-actions="{ row }">
              <Space spacing="compact">
                <Button v-if="asScan(row).hasReport" size="sm" variant="ghost" @click="openResult(asScan(row))">
                  {{ t('history.action.view') }}
                </Button>
                <Button v-if="asScan(row).kind === 'scan' && (asScan(row).outcome === 'paused' || asScan(row).outcome === 'interrupted')" size="sm" variant="ghost" @click="navigate('scanning')">
                  {{ t('history.action.resume') }}
                </Button>
                <Button v-if="asScan(row).kind === 'rootfs-apply' || (asScan(row).kind === 'cleanup-apply' && ['paused', 'interrupted', 'failed', 'cancelled'].includes(asScan(row).outcome))" size="sm" variant="ghost" @click="openRecovery(asScan(row))">
                  {{ t(asScan(row).kind === 'cleanup-apply' ? 'review.recheck' : 'history.action.view') }}
                </Button>
                <Button
                  v-if="asScan(row).kind === 'scan' && ['partial', 'superseded', 'failed'].includes(asScan(row).outcome)"
                  size="sm"
                  variant="ghost"
                  @click="whyRow = asScan(row)"
                >
                  {{ t('history.action.why') }}
                </Button>
              </Space>
            </template>
          </ReclaimGrid>
        </Surface>
      </template>

      <RootfsHistorySection
        v-if="rootfsApplies.length"
        :applies="rootfsApplies"
        :can-clean-up="rootfsCanCleanUp"
        @check-and-clean-up="checkAndCleanUp"
      />
    </template>

    <ReviewDialog v-if="recoveryJob?.kind === 'cleanup-apply'" v-model:open="recoveryOpen" :volume="recoveryJob.filesystems[0]?.mount.path" :paths="recoveryPaths" />
    <Dialog v-if="recoveryJob?.kind === 'rootfs-apply'" v-model:open="recoveryOpen" :aria-describedby="undefined" :title="t('common.job.kind.rootfs-apply')">
      <RootfsApplyStep :open="recoveryOpen" :job-id="recoveryJob.job_id" follow-only @close="recoveryOpen = false" @check-again="recoveryOpen = false; openRootfsView()" />
    </Dialog>

    <Dialog v-model:open="resultOpen" :aria-describedby="undefined" :title="`${t('history.action.view')} · ${resultId ?? ''}`">
      <LoadingArea v-if="resultLoading" :text="t('common.loading')" />
      <StateView v-else-if="resultError" kind="error" :error="resultError" />
      <div v-else-if="selectedResult" data-testid="history-result" :data-report-job-id="selectedResult.job_id">
        <p>{{ selectedResult.job_id }} · {{ outcomeLabel(selectedResult.completion === 'complete' ? 'completed' : selectedResult.completion) }}</p>
        <section v-for="(filesystem, index) in selectedResult.filesystems" :key="index">
          <h3>{{ filesystem.mount.path }}</h3>
          <p>{{ formatDateTime(filesystem.scan_finished) }}</p>
          <dl>
            <dt>{{ t('history.table.scope') }}</dt><dd>{{ filesystem.targets.map(target => target.path).join(', ') }}</dd>
            <dt>{{ t('history.table.entries') }}</dt><dd>{{ formatCount(filesystem.totals?.files) }}</dd>
            <dt>{{ t('explore.table.size') }}</dt><dd>{{ formatBytes(filesystem.totals?.physical) }}</dd>
          </dl>
        </section>
        <p v-for="(issue, index) in selectedResult.errors" :key="index">{{ code('error', issue.code) }}</p>
      </div>
      <template #footer><Button @click="resultOpen = false">{{ t('common.close') }}</Button></template>
    </Dialog>

    <Dialog
      v-model:open="whyOpen"
      :aria-describedby="undefined"
      :title="t('history.why.title', { outcome: whyRow ? outcomeLabel(whyRow.outcome) : '' })"
    >
      <p>{{ whyText }}</p>
      <template #footer>
        <Button @click="whyRow = null">{{ t('common.close') }}</Button>
      </template>
    </Dialog>

    <Dialog v-model:open="clearOpen" :aria-describedby="undefined" :title="t('history.results.clear.title')">
      <p>{{ t('history.results.clear.body') }}</p>
      <template #footer>
        <Button @click="clearOpen = false">{{ t('common.cancel') }}</Button>
        <Button variant="primary" :loading="clearing" @click="clearCache">{{ t('history.results.clear.confirm') }}</Button>
      </template>
    </Dialog>
  </div>
</template>

<style scoped>
.history-page {
  /* A fill page (registry): the cards stay, the table takes the height left
     and its DataGrid scrolls the rows under a header and footer that stay. */
  display: flex;
  height: 100%;
  min-height: 0;
  flex-direction: column;
  gap: var(--ui-section-gap);
  /* Too short a window for the cards and a usable table (or with the
     system-partition section below): the page scrolls instead. */
  overflow-y: auto;
}
.history-page > .history-table {
  display: flex;
  flex: 1 1 auto;
  flex-direction: column;
  min-height: 240px;
}
.history-cards {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 320px;
  gap: 16px;
}
.card-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}
.card-head h2,
.results-title {
  font-size: var(--ui-font-size-lg);
  font-weight: 700;
}
.volume-pick {
  max-width: 180px;
}
.muted {
  color: var(--ui-text-muted);
  font-size: var(--ui-font-size-sm);
}
.results {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 10px 0;
}
.results > div {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  font-size: var(--ui-font-size);
}
.results dt {
  flex: none;
  color: var(--ui-text-secondary);
  white-space: nowrap;
}
.results dd {
  min-width: 0;
  overflow: hidden;
  font-weight: 600;
  text-align: right;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.results-card .muted {
  margin: 4px 0 0;
}
.results-card > button {
  margin-top: 12px;
}
</style>
