<script setup lang="ts">
// Snapshot page: which snapshot holds the space and
// what deleting a set frees, as four numbered steps from top to bottom.
// Read-only: this page computes, DSM deletes.
import PartialHint from '@/components/PartialHint.vue'
import ScanBadge from '@/components/ScanBadge.vue'
import { reportCompleteness, visibleConsistency } from '@/components/reportCompleteness'
import { reportNotes } from '@/components/reportNotes'
import ScanNotes from '@/components/ScanNotes.vue'
import { PageInfo } from '@synopackageland/sdk/vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, onScopeDispose, ref, watch } from 'vue'
import { Badge, Button, Callout, CellText, EmptyState, Select, LoadingArea, Statistic, Surface, writeClipboard } from '@synopackageland/ui'
import { PageActions } from '@synopackageland/sdk/vue'
import { Camera, ScanSearch } from '@lucide/vue'
import { api, ApiError } from '@/api'
import { targetSnapshotStates, inventorySource, type DsmInventory, type ShareSnapshotStates } from '@/dsm/snapshotInventory'
import StateView from '@/components/states/StateView.vue'
import { code, formatBytes, formatDateTime, formatDuration, joinParts, locale, t } from '@/i18n'
import { jobs, pollNow } from '@/stores/jobs'
import { authorizeRootWork } from '@/stores/authorization'
import { apiErrorText } from '@/components/states/apiErrorText'
import { isActiveJob } from '@/stores/jobStates'
import { openNewScan, openJob } from '@/stores/nav'
import { selectedVolume } from '@/stores/volumes'
import { newerScannedShares } from '@/components/shareReports'
import type { SnapshotsDocument, TopDocument } from '@/types'
import KnownHeldCard from './KnownHeldCard.vue'
import { snapshotHeld } from './knownHeld'
import SnapshotTable from './SnapshotTable.vue'
import { describeTarget, snapshotManager, targetGroups, type NameLabels, type TargetSpace } from './snapshotTargets'

type Result = SnapshotsDocument['results'][number]
type Share = Result['shares'][number]
type Snapshot = Share['snapshots'][number]

/** The volume-level question Explore and Packages ask too, so all three share one cached answer. */
const TOP_LIMIT = 200

const doc = ref<SnapshotsDocument | null>(null)
const error = ref<ApiError | null>(null)
const loading = ref(false)
const sharePath = ref('')
const ownResults = ref(new Map<string, Result>())
const selectedIds = ref<string[]>([])
const simulation = ref<Share['simulation'] | null>(null)
const simulating = ref(false)
/** Without per-snapshot data: the same report's volume-level rows, for what snapshots hold of them. */
const top = ref<TopDocument | null>(null)
const axisDate = new Intl.DateTimeFormat(locale, { month: 'numeric', day: 'numeric' })
/** DSM's own names and lock states; null until read, or outside DSM. */
const inventory = ref<DsmInventory | null>(null)
const inventoryLoading = ref(false)
const shareStatesLoading = ref(false)
const shareStates = ref<ShareSnapshotStates | null>(null)

const result = computed<Result | null>(() => ownResults.value.get(sharePath.value) ?? doc.value?.results[0] ?? null)
const report = computed(() => result.value?.report ?? null)
const analysisSource = computed(() => report.value ?? top.value?.results[0]?.report ?? null)
const shares = computed(() => doc.value?.results[0]?.shares ?? [])
const share = computed<Share | null>(() => shares.value.find(s => s.share.path === sharePath.value) ?? shares.value[0] ?? null)
const snapshotsNotMaterialized = computed(() => report.value?.coverage.snapshots_not_materialized ?? 0)
const unanalysed = computed(() => shares.value.map(share => ({
  path: share.share.path,
  snapshots: share.snapshots.filter(snapshot => !snapshot.materialized && !snapshot.pending_deletion),
})).filter(group => group.snapshots.length))
const analysing = ref(false)
const analysisError = ref('')
const busy = computed(() => jobs.list.some(isActiveJob))
async function analyseAll() {
  if (!selectedVolume.value || !analysisSource.value || analysing.value || busy.value) return
  analysing.value = true
  analysisError.value = ''
  try {
    if (!(await authorizeRootWork())) return
    const response = await api('scan.start', { targets: [selectedVolume.value.mount], mode: 'gentle', snapshots: 'all', snapshot_report: analysisSource.value.job_id })
    if (response.outcome === 'scan_in_progress') { analysisError.value = t('state.busy.title'); return }
    if (response.job_id) openJob({ job_id: response.job_id, kind: 'scan' })
    pollNow()
  } catch (error) { analysisError.value = apiErrorText(error) }
  finally { analysing.value = false }
}
const nameLabels: NameLabels = {
  unnamedLun: id => t('snapshots.target.unnamedLun', { id }),
  vmmImage: t('snapshots.target.vmmImage'),
  vmmDisk: t('snapshots.target.vmmDisk'),
  tiering: t('snapshots.target.tiering'),
  unnamed: id => t('snapshots.target.unnamed', { id }),
}
function spaceText(space: TargetSpace) {
  if (space.certainty === 'unknown') return t('snapshots.target.spaceUnknown')
  if (space.certainty === 'upTo') return t('snapshots.held.upTo', { bytes: formatBytes(space.upper) })
  if (space.certainty === 'range') return t('snapshots.selection.range', { from: formatBytes(space.bytes), to: formatBytes(space.upper) })
  return formatBytes(space.bytes)
}
const groups = computed(() => targetGroups(shares.value, inventory.value, { ...nameLabels, space: spaceText }))
const shareOptions = computed(() => groups.value.map(group => ({
  label: t(`snapshots.target.kind.${group.kind}`),
  options: group.options.map(option => ({ label: option.label, value: option.value })),
})))
const optionCount = computed(() => groups.value.reduce((sum, group) => sum + group.options.length, 0))
const target = computed(() => (share.value ? describeTarget(share.value, inventory.value, nameLabels) : null))
const targetName = (path: string) => {
  const found = shares.value.find(item => item.share.path === path)
  return found ? describeTarget(found, inventory.value, nameLabels).name : path
}
/** DSM's lock states for the open target's snapshots. */
const snapshotStates = computed(() => {
  if (!target.value) return null
  if (target.value.kind === 'share') return shareStates.value
  return share.value ? targetSnapshotStates(share.value, inventory.value) : null
})
const manager = computed(() => snapshotManager(target.value?.kind ?? 'other'))
const targetError = computed(() => doc.value?.errors[0] ?? null)
const needsSnapshots = computed(() => targetError.value?.code === 'no_report')
/** Saved reports exist but in another report format (SPEC GAP-21): only a new scan helps. */
const incompatible = computed(() => targetError.value?.code === 'report_incompatible')
const lastScanSeconds = computed(() => {
  const ms = doc.value?.scan_hints?.[0]?.last_scan?.elapsed_ms
  return ms == null ? null : ms / 1000
})
const volumeHasReport = computed(() => (selectedVolume.value?.reports ?? 0) > 0)
const known = computed(() => snapshotHeld(top.value?.results[0], TOP_LIMIT))

/** Newest first, as DSM lists snapshots. */
const snapshots = computed<Snapshot[]>(() =>
  [...(share.value?.snapshots ?? [])].sort((a, b) => timeKey(b) - timeKey(a)),
)
const completeness = computed(() => reportCompleteness(report.value, doc.value?.completion, !!doc.value?.errors.length))
const heldUnknown = computed(() => share.value?.unknown_bytes ?? 0)
/**
 * No snapshot of the folder holds space by itself, but its snapshots may hold some: the space
 * is shared by several snapshots or its owner is unknown. Only a range answers.
 */
const sharedOnly = computed(() => !!share.value && !share.value.held_only_by_snapshots_bytes && heldUnknown.value > 0
  && !snapshots.value.some(snapshot => snapshot.materialized && (snapshot.exclusive_bytes ?? 0) > 0))
/** Every folder is like that: the scan cannot say exactly what snapshots hold, only the most. */
const unknownTotal = computed(() =>
  shares.value.some(s => s.held_only_by_snapshots_bytes > 0) ? 0 : shares.value.reduce((sum, s) => sum + (s.unknown_bytes ?? 0), 0),
)
const heldValue = computed(() => {
  if (!share.value?.snapshots.some(snapshot => snapshot.materialized)) return t('common.unknown')
  const held = share.value?.held_only_by_snapshots_bytes ?? 0
  return !held && heldUnknown.value ? t('snapshots.held.upTo', { bytes: formatBytes(heldUnknown.value) }) : formatBytes(held)
})
/** The answer has only a bound: nothing is known to be freed, but up to this much may be. */
const upToOnly = computed(() => !!simulation.value && !simulation.value.exact && !simulation.value.freed_bytes && (simulation.value.upper_bound_bytes ?? 0) > 0)
const pageNotes = computed(() => [
  ...(completeness.value.warn ? [] : reportNotes(report.value, selectedVolume.value?.shares ?? [], doc.value?.completion)),
  heldUnknown.value ? t('snapshots.held.unknownNote', { bytes: formatBytes(heldUnknown.value) }) : '',
])
/** Why a selection's answer is not exact; the bound itself stays in view. */
const simulationNotes = computed(() => {
  const answer = simulation.value
  if (!answer || answer.exact) return []
  return [
    ...(answer.unknown_bytes ? [t('snapshots.selection.unknown', { bytes: formatBytes(answer.unknown_bytes) })] : []),
    ...answer.inexact_reasons.map(reason => code('inexact', reason)),
  ]
})
const selectedRange = computed(() => {
  const chosen = snapshots.value.filter(snapshot => selectedIds.value.includes(String(snapshot.subvolume_id)))
  if (!chosen.length) return null
  const from = shortDate(chosen[chosen.length - 1]!.otime)
  const to = shortDate(chosen[0]!.otime)
  return from === to ? from : t('snapshots.selection.range', { from, to })
})
const heldFiles = computed(() => share.value?.held_files ?? null)
/** A 0 B row says nothing about held space. */
const fileRows = computed(() =>
  (heldFiles.value?.files ?? []).filter(file => file.bytes > 0).map(file => ({
    id: file.ino,
    path: file.path?.path ?? file.live_path?.path ?? t('snapshots.heldFiles.unknownName', { ino: file.ino }),
    snapshot: file.snapshot.path,
    bytes: file.bytes,
    inLive: file.in_live,
  })),
)
const filesUnsized = computed(() => !fileRows.value.length && (heldFiles.value?.count ?? 0) > 0)
const filesMore = computed(() => (filesUnsized.value ? 0 : Math.max(0, (heldFiles.value?.count ?? 0) - fileRows.value.length)))
const updating = computed(() =>
  jobs.list.some(
    job => isActiveJob(job) && job.filesystems?.some(fs => fs.mount.path === selectedVolume.value?.mount.path),
  ),
)
const reportTitle = computed(() => {
  if (!report.value) return undefined
  const reasons = (report.value.change_reasons ?? []).map(reason => code('change', reason.kind))
  return joinParts([code('consistency', visibleConsistency(report.value) ?? 'partial'), ...reasons], locale)
})
const fileColumns = [
  { key: 'path', sortable: true, label: t('snapshots.heldFiles.column.path'), minWidth: 120 },
  { key: 'snapshot', sortable: true, label: t('snapshots.heldFiles.column.snapshot'), minWidth: 120 },
  { key: 'bytes', sortable: true, label: t('snapshots.heldFiles.column.bytes'), align: 'right' as const, width: '84px' },
]

function timeKey(snapshot: Snapshot) {
  return snapshot.otime ? Date.parse(snapshot.otime) : 0
}
function shortDate(otime: string | null) {
  return otime ? axisDate.format(new Date(otime)) : t('common.unknown')
}

/** The folder whose snapshots hold the most, then may hold the most, then hold any space alone. */
function defaultShare(items: Share[]) {
  const rank = (item: Share) => [
    item.held_only_by_snapshots_bytes,
    item.unknown_bytes ?? 0,
    item.snapshots.some(snapshot => snapshot.materialized && (snapshot.exclusive_bytes ?? 0) > 0) ? 1 : 0,
  ]
  return items.reduce<Share | null>((best, candidate) => {
    if (!best) return candidate
    const [a, b] = [rank(candidate), rank(best)]
    const differs = a.findIndex((value, index) => value !== b[index])
    return differs >= 0 && a[differs]! > b[differs]! ? candidate : best
  }, null)
}

let loadController: AbortController | undefined
async function load(mount: string) {
  loadController?.abort()
  inventoryController?.abort()
  const controller = loadController = new AbortController()
  loading.value = true
  top.value = null
  ownResults.value = new Map()
  try {
    const saved = await api('snapshots.list', { target: { path: mount } }, { signal: controller.signal })
    if (controller.signal.aborted) return
    const answer = { ...saved, results: saved.results.map(result => ({ ...result, shares: [...result.shares] })) }
    const history = await api('history.list', {}, { signal: controller.signal }).catch(() => null)
    if (controller.signal.aborted) return
    const paths = history ? newerScannedShares(history, mount, answer.results[0]?.report.scan_finished) : []
    // Read only saved share reports, never start a volume scan to populate this page.
    const own = await Promise.all(paths.map(path => api('snapshots.list', { target: path }, { signal: controller.signal }).catch(() => null)))
    if (controller.signal.aborted) return
    const targetShares = new Map(answer.results.flatMap(result => result.shares).map(share => [share.share.path, share]))
    for (const document of own) for (const result of document?.results ?? []) for (const share of result.shares) {
      targetShares.set(share.share.path, share)
      ownResults.value.set(share.share.path, result)
    }
    if (own.some(document => document?.results.length) && !answer.results.length) {
      const first = own.find(document => document?.results.length)!
      answer.results = [{ ...first.results[0]!, shares: [...targetShares.values()] }]
      answer.errors = answer.errors.filter(error => error.code !== 'no_report')
      answer.completion = first.completion
    } else if (answer.results[0]) answer.results[0].shares = [...targetShares.values()]
    if (controller.signal.aborted) return
    doc.value = answer
    error.value = null
    sharePath.value = defaultShare(doc.value.results[0]?.shares ?? [])?.share.path ?? ''
    // A report without per-snapshot data still says what snapshots hold of each row.
    // So does a report whose snapshots hold nothing alone.
    void loadInventory()
    if ((needsSnapshots.value && volumeHasReport.value) || unknownTotal.value || (shares.value.length > 0 && shares.value.every(share => share.snapshots.every(snapshot => !snapshot.materialized)))) {
      const answer = await loadTop(mount, controller.signal)
      if (!controller.signal.aborted) top.value = answer
    }
  } catch (caught) {
    if (controller.signal.aborted) return
    doc.value = null
    error.value = caught instanceof ApiError ? caught : new ApiError('unknown')
  } finally {
    if (!controller.signal.aborted) loading.value = false
  }
}

/** The volume-level rows; null when they cannot be read, which leaves only the scan offer. */
async function loadTop(mount: string, signal: AbortSignal) {
  try {
    return await api('top.list', { target: mount, kind: 'all', by: 'reclaimable', limit: TOP_LIMIT }, { signal })
  } catch {
    return null
  }
}

/** Abort superseded reads; the SDK owns their request budget. */
let inventoryController: AbortController | undefined
async function loadInventory() {
  inventoryController?.abort()
  const controller = inventoryController = new AbortController()
  inventoryLoading.value = true
  inventory.value = null
  try {
    const answer = await inventorySource().inventory(controller.signal).catch(() => null)
    if (!controller.signal.aborted) inventory.value = answer
  } finally {
    if (!controller.signal.aborted) inventoryLoading.value = false
  }
}
onScopeDispose(() => { loadController?.abort(); inventoryController?.abort() })

/** DSM's lock states of a shared folder's snapshots, read when it opens. */
watch(() => (target.value?.kind === 'share' ? share.value?.share.path : undefined), async (name, _old, onCleanup) => {
  const controller = new AbortController()
  onCleanup(() => controller.abort())
  shareStates.value = null
  shareStatesLoading.value = !!name
  if (!name) return
  try {
    const answer = await inventorySource().shareSnapshots(name, controller.signal).catch(() => null)
    if (!controller.signal.aborted) shareStates.value = answer
  } finally {
    if (!controller.signal.aborted) shareStatesLoading.value = false
  }
})

/** Counts simulate requests, so an answer that returns after a newer selection is dropped. */
let simulateRequest = 0

/** Freed bytes of the selection, answered from the report (SPEC 5.14). */
async function simulate(ids: string[]) {
  const mount = selectedVolume.value?.mount.path
  if (!mount) return
  const selector = ids
    .map(Number)
    .sort((a, b) => a - b)
    .map(id => `id:${id}`)
    .join(',')
  const request = ++simulateRequest
  try {
    const path = share.value?.share.path
    const queryTarget = path && ownResults.value.has(path) && target.value?.kind === 'share' ? (path.startsWith('/') ? path : `${mount}/${path}`) : mount
    const answer = await api('snapshots.list', { target: { path: queryTarget }, simulate: selector })
    if (request !== simulateRequest) return
    simulation.value = answer.results[0]?.shares.find(s => s.share.path === share.value?.share.path)?.simulation ?? null
  } catch {
    if (request === simulateRequest) simulation.value = null
  } finally {
    if (request === simulateRequest) simulating.value = false
  }
}

let timer: ReturnType<typeof setTimeout> | undefined
watch(selectedIds, ids => {
  clearTimeout(timer)
  simulateRequest++
  simulation.value = null
  if (!ids.length) {
    simulating.value = false
    return
  }
  simulating.value = true
  timer = setTimeout(() => void simulate([...ids]), 400)
})

watch(
  () => selectedVolume.value?.mount.path,
  mount => {
    loadController?.abort()
    inventoryController?.abort()
    inventoryLoading.value = false
    selectedIds.value = []
    simulation.value = null
    sharePath.value = ''
    if (mount) void load(mount)
  },
  { immediate: true },
)

function reload() {
  const mount = selectedVolume.value?.mount.path
  if (mount) void load(mount)
}

function openSnapshotManager() {
  const syno = window.SYNO?.SDS as { AppLaunch?: (id: string) => void } | undefined
  syno?.AppLaunch?.(manager.value.appId)
}

function copyDetails() {
  void writeClipboard(
    JSON.stringify({ code: error.value?.code, exit: error.value?.exit, detail: error.value?.detail }),
  )
}
</script>

<template>
  <ScanBadge v-if="doc && report" :report="report" :consistency="completeness.warn ? 'partial' : undefined" :details="reportTitle" :notes="share ? pageNotes : undefined" />
  <PageActions>
    <Button v-if="!loading && analysisSource && share && !unanalysed.length" data-testid="snapshot-refresh" :loading="analysing" :disabled="busy || analysing" @click="analyseAll">{{ t('snapshots.analyseAll') }}</Button>
  </PageActions>

  <div class="snapshot-page">
    <LoadingArea v-if="inventoryLoading" data-testid="dsm-inventory-loading" :text="t('common.loading')" />
    <LoadingArea v-if="loading" :text="t('common.loading')" />

    <StateView v-else-if="error" :kind="error.code === 'busy' ? 'busy' : 'error'" :codes="[{ set: 'api', value: error.code }]">
      <Button @click="reload">{{ t('common.retry') }}</Button>
      <Button @click="copyDetails">{{ t('common.copyDetails') }}</Button>
    </StateView>

    <EmptyState v-else-if="incompatible" :title="code('error', 'report_incompatible')" :icon="ScanSearch">
      <template #actions>
        <Button variant="primary" @click="openNewScan">{{ t('state.noReport.scan') }}</Button>
      </template>
    </EmptyState>

    <StateView v-else-if="needsSnapshots && !volumeHasReport" kind="no_report" :last-scan-seconds="lastScanSeconds">
      <Button variant="primary" @click="openNewScan">{{ t('state.noReport.scan') }}</Button>
    </StateView>

    <template v-else-if="needsSnapshots">
      <Callout :dismissible="false" data-testid="snapshot-detail-needed" tone="info" :title="t('snapshots.detailNeeded.title')">
        <span>{{ t(analysisSource ? 'snapshots.unanalysed.help' : 'snapshots.detailNeeded.body') }}</span>
        <template #actions>
          <Button size="sm" @click="openNewScan">{{ t('snapshots.noSnapshots.action') }}</Button>
          <Button v-if="analysisSource" size="sm" variant="primary" :loading="analysing" :disabled="busy || analysing" @click="analyseAll">{{ t('snapshots.analyseAll') }}</Button>
        </template>
      </Callout>
      <p v-if="analysisError" role="alert">{{ analysisError }}</p>
      <KnownHeldCard :known="known" />
    </template>

    <EmptyState
      v-else-if="doc && !share"
      :title="t('snapshots.noShares.title')"
      :description="t('snapshots.noShares.body')"
      :icon="Camera"
    />

    <template v-else-if="share && doc">
      <PartialHint
        v-if="completeness.warn"
        :details="[snapshotsNotMaterialized ? t('snapshots.coverage.incomplete', { count: snapshotsNotMaterialized }) : t('state.partial.body')]"
      />
      <Callout v-if="updating" tone="info" :title="t('snapshots.banner.updating', { time: formatDateTime(report?.scan_finished) })" />
      <PageInfo v-if="!report" data-testid="page-info" :notes="pageNotes" />
      <Surface v-if="unanalysed.length" data-testid="unanalysed-snapshots" class="unanalysed">
        <h2>{{ t('snapshots.unanalysed.title') }}</h2>
        <p>{{ t('snapshots.unanalysed.help') }}</p>
        <details v-for="group in unanalysed" :key="group.path">
          <summary>{{ targetName(group.path) }} · {{ t('snapshots.unanalysed.count', { count: group.snapshots.length }) }}</summary>
          <ul>
            <li v-for="snapshot in group.snapshots" :key="snapshot.subvolume_id">
              <span>{{ snapshot.name.path }}</span>
              <time v-if="snapshot.otime" :datetime="snapshot.otime">{{ formatDateTime(snapshot.otime) }}</time>
            </li>
          </ul>
        </details>
        <Button :loading="analysing" :disabled="busy || analysing" @click="analyseAll">{{ t('snapshots.analyseAll') }}</Button>
        <p v-if="analysisError" role="alert">{{ analysisError }}</p>
      </Surface>
      <Callout v-if="result?.snapshot_analysis" tone="info" :title="t('snapshots.analysis.done', { time: formatDuration(result.snapshot_analysis.elapsed_ms / 1000) })">
        {{ t('snapshots.analysis.retained', { time: formatDateTime(report?.scan_finished) }) }}
        <p v-if="result.snapshot_analysis.changed_live_subvolumes?.length">{{ t('snapshots.analysis.changed') }}</p>
      </Callout>
      <Callout v-if="unknownTotal" :dismissible="false" data-testid="snapshot-unknown" tone="info" :title="t('snapshots.unknown.title')">
        <span>{{ joinParts([t('snapshots.unknown.body', { bytes: formatBytes(unknownTotal) }), snapshotsNotMaterialized ? t('snapshots.unknown.unread', { count: snapshotsNotMaterialized }) : ''], locale) }}</span>
        <template #actions>
          <Button v-if="snapshotsNotMaterialized" size="sm" variant="primary" @click="openNewScan">{{ t('snapshots.noSnapshots.action') }}</Button>
        </template>
      </Callout>

      <ol class="steps">
        <li>
          <Surface border class="step">
            <h2><span class="step-number">1</span>{{ t('snapshots.step.share') }}</h2>
            <div class="head">
              <div class="target">
                <Select
                  :model-value="share.share.path"
                  class="share"
                  :aria-label="t('snapshots.share.label')"
                  :options="shareOptions"
                  :search="optionCount > 10"
                  :disabled="optionCount < 2"
                  @update:model-value="sharePath = $event"
                />
                <span v-if="target" class="target-kind" data-testid="target-kind">
                  {{ joinParts([t(`snapshots.target.type.${target.kind}`), target.id ?? ''], locale) }}
                </span>
              </div>
              <Statistic class="held" :label="t('snapshots.held.label')" :value="heldValue" />
            </div>
          </Surface>
        </li>

        <li>
          <Surface border class="step">
            <h2><span class="step-number">2</span>{{ t('snapshots.step.select') }}</h2>
            <Callout
              v-if="sharedOnly"
              data-testid="snapshot-shared"
              tone="info"
              dismiss-key="snapshot-shared"
              :title="t('snapshots.shared.title')"
            >
              {{ t('snapshots.shared.body') }}
            </Callout>
            <LoadingArea v-if="inventoryLoading || shareStatesLoading" data-testid="dsm-snapshot-loading" :text="t('common.loading')" />
            <SnapshotTable v-else v-model="selectedIds" :snapshots="snapshots" :states="snapshotStates" :shared-only="sharedOnly" />
          </Surface>
        </li>

        <li>
          <Surface border class="step result-card" :class="{ filled: selectedIds.length > 0 }">
            <h2><span class="step-number">3</span>{{ t('snapshots.step.frees') }}</h2>
            <template v-if="selectedIds.length">
              <Statistic
                :label="t('snapshots.selection.title', { count: selectedIds.length })"
                :value="simulation ? formatBytes(upToOnly ? simulation.upper_bound_bytes : simulation.freed_bytes) : '—'"
                :note="t(simulation?.exact ? 'snapshots.selection.freesExact' : upToOnly ? 'snapshots.selection.freesUpTo' : 'snapshots.selection.frees')"
              />
              <span v-if="selectedRange" class="result-range">{{ selectedRange }}</span>
              <LoadingArea v-if="simulating" :text="t('snapshots.selection.computing')" />
              <template v-else-if="simulation">
                <span v-if="simulation.exact" class="result-exact">{{ t('snapshots.selection.exact') }}</span>
                <span v-else-if="upToOnly">{{ t('snapshots.selection.unsure') }}</span>
                <span v-else>{{ joinParts([t('snapshots.selection.inexact'), t('snapshots.selection.upperBound', { bytes: formatBytes(simulation.upper_bound_bytes) })], locale) }}</span>
                <ScanNotes :notes="simulationNotes" />
              </template>
            </template>
            <p v-else class="hint">{{ t('snapshots.selection.none') }}</p>
          </Surface>
        </li>

        <li>
          <Surface border class="step">
            <h2><span class="step-number">4</span>{{ t('snapshots.step.delete') }}</h2>
            <div class="delete">
              <Button @click="openSnapshotManager">
                {{ t('snapshots.dsm.button', { place: code('manage_in', manager.placeId) }) }}
              </Button>
              <span class="hint">{{ t('snapshots.dsm.note') }}</span>
            </div>
          </Surface>
        </li>
      </ol>

      <Surface class="files-card" padding="md">
        <div class="card-title files-title">
          <strong>{{ t('snapshots.heldFiles.title') }}</strong>
          <span class="hint">{{ t('snapshots.heldFiles.description') }}</span>
        </div>
        <p v-if="filesUnsized" class="files-more">{{ t('snapshots.heldFiles.unsized', { count: heldFiles?.count ?? 0 }) }}</p>
        <ReclaimGrid v-else table-id="snapshot-files-v2" :columns="fileColumns" :rows="fileRows" row-key="id" :empty-label="t('snapshots.heldFiles.empty')">
          <template #cell-path="{ row }">
            <CellText layout="inline" :data-ui-tooltip-text="String(row.path)" :text="String(row.path)" wrap>
              <template v-if="row.inLive" #secondary><Badge tone="neutral">{{ t('snapshots.heldFiles.inLive') }}</Badge></template>
            </CellText>
          </template>
          <template #cell-snapshot="{ row }"><CellText :text="String(row.snapshot)" wrap /></template>
          <template #cell-bytes="{ row }"><CellText class="tabular-nums" :text="formatBytes(row.bytes as number)" /></template>
        </ReclaimGrid>
        <p v-if="filesMore" class="files-more">{{ t('snapshots.heldFiles.more', { count: filesMore }) }}</p>
        <p v-if="heldFiles && !heldFiles.complete" class="files-more">{{ t('snapshots.heldFiles.notTallied') }}</p>
      </Surface>
      <KnownHeldCard v-if="unknownTotal && top" :known="known" />
    </template>
  </div>
</template>

<style scoped>
.unanalysed { padding: var(--ui-space-4, 16px); display: grid; gap: 12px; }
.unanalysed h2 { margin: 0; font-size: var(--ui-font-size-lg); }
.unanalysed p { margin: 0; }
.unanalysed summary { cursor: pointer; overflow-wrap: anywhere; }
.unanalysed ul { max-height: 240px; overflow: auto; padding-left: 24px; }
.unanalysed li { overflow-wrap: anywhere; margin-block: 6px; }
.unanalysed time { margin-inline-start: 12px; color: var(--ui-text-secondary); }
.unanalysed > button { justify-self: start; }

.snapshot-page {
  display: flex;
  flex-direction: column;
  gap: var(--ui-section-gap);
  container-type: inline-size;
}
.steps {
  display: grid;
  gap: 12px;
  margin: 0;
  padding: 0;
  list-style: none;
}
/* A wide table scrolls inside its card instead of widening the page. */
.steps > li {
  min-width: 0;
}
.step {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 8px;
  height: 100%;
  font-size: var(--ui-font-size);
  color: var(--ui-text);
}
.step h2 {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0;
  font-size: var(--ui-font-size-lg);
  font-weight: 600;
}
.step-number {
  display: inline-grid;
  width: 22px;
  height: 22px;
  flex: none;
  place-items: center;
  border-radius: 50%;
  background: var(--ui-primary);
  color: var(--ui-primary-foreground);
  font-size: var(--ui-font-size-sm);
}
.step p {
  margin: 0;
}
.head {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  justify-content: space-between;
  gap: 16px;
}
.target {
  display: grid;
  min-width: 0;
  gap: 4px;
}
.share {
  width: 420px;
  max-width: 100%;
}
.target-kind {
  font-size: var(--ui-font-size-sm);
  color: var(--ui-text-secondary);
}
.hint {
  font-size: var(--ui-font-size);
  color: var(--ui-text-secondary);
}
.result-card.filled {
  border-color: var(--ui-primary);
}
.result-range {
  color: var(--ui-text-secondary);
}
.result-exact {
  color: var(--ui-success);
}
.delete {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
}
.card-title {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 10px;
}
.card-title strong {
  font-size: var(--ui-font-size-lg);
}
.files-title {
  flex-wrap: wrap;
}
.files-more {
  margin: 0;
  padding-top: 8px;
  font-size: var(--ui-font-size);
  color: var(--ui-text-secondary);
}
</style>
