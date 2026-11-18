<script setup lang="ts">
// Recycle Bin page: the Recycle Bin of every shared
// folder on the selected volume. Selecting a share shows its detail beside
// the list: holders and the oldest items. Reclaim moves its cleanups into
// these Recycle Bins and never empties one: the page shows what it moved
// since the scan and where DSM empties them. No treemap.
import PartialHint from '@/components/PartialHint.vue'
import ScanBadge from '@/components/ScanBadge.vue'
import PageStatus from '@/components/PageStatus.vue'
import { reportCompleteness, visibleConsistency } from '@/components/reportCompleteness'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import ScanNotes from '@/components/ScanNotes.vue'
import { reportNotes } from '@/components/reportNotes'
import { cleanupUnavailableText } from '@/components/cleanupUnavailable'
import { computed, reactive, ref, watch, type Directive } from 'vue'
import { Badge, Button, Callout, CellText, EmptyState, ListDetail, LoadingArea, Tooltip, Space, writeClipboard } from '@synopackageland/ui'
import { PageActions } from '@synopackageland/sdk/vue'
import type { DataGridColumn } from '@synopackageland/ui'
import { code, formatBytes, formatCount, formatCompactDateTime, formatDateTime, formatRelative, joinParts, locale, t } from '@/i18n'
import { api, ApiError } from '@/api'
import type { HistoryDocument, Path, RecycleDocument } from '@/types'
import ReportPicker from '@/components/ReportPicker.vue'
import { reportsCovering, scopeText } from '@/components/reportChoice'
import StateView from '@/components/states/StateView.vue'
import { SHARE_METRIC_PAGE, newerScannedShares } from '@/components/shareReports'
import { openInFileStation } from '@/components/fileStation'
import { FolderOpen } from '@lucide/vue'
import { jobs } from '@/stores/jobs'
import { navigate, openNewScan } from '@/stores/nav'
import { selectedVolume } from '@/stores/volumes'
import {
  canLoadMore,
  nextOldestLimit,
  OLDEST_MAX,
  OLDEST_PAGE,
  recycleTarget,
  shareAnswersAsVolume,
  shareTarget,
  sortOldest,
  type OldestItem,
  type RecycleShare,
} from './recycle'

type Status = 'loading' | 'ready' | 'error'
type DisplayCode = { set: string; value: string }

const status = ref<Status>('loading')
const doc = ref<RecycleDocument | null>(null)
const error = ref<ApiError | null>(null)
const errorCodes = ref<DisplayCode[]>([])
const copiedErrorMessage = ref('')
const expandedKey = ref<string | null>(null)
const oldest = reactive<Record<string, OldestItem[]>>({})
const limits = reactive<Record<string, number>>({})
const oldestDocuments = reactive<Record<string, RecycleDocument>>({})
const oldestRequestErrors = reactive<Record<string, DisplayCode[]>>({})
const oldestApiErrors = reactive<Record<string, ApiError>>({})
const oldestUnavailable = reactive<Record<string, boolean>>({})
const oldestLoading = ref(false)
/** The volume has no report: the page shows the shares scanned on their own. */
const sharesOnly = ref(false)
/** The volume's no-report answer the shares-only view is rebuilt from. */
const sharesOnlyBase = ref<RecycleDocument | null>(null)
/** Every share a share scan left a report for; its answers load one page at a time. */
const sharePaths = ref<Path[]>([])
const shareAnswers = ref<(RecycleDocument | null)[]>([])
/** The volume's saved scans, for the report picker and its scope words. */
const history = ref<HistoryDocument | null>(null)
/** The saved report the user chose by its job ID; null answers each share from its newest report. */
const chosenReport = ref<string | null>(null)

let loadRequestId = 0
let oldestRequestId = 0

const volume = computed(() => selectedVolume.value)
// A volume this app cannot clean still shows its Recycle Bins; nothing on it can be selected.
const cleanupUnavailable = computed(() => volume.value?.capabilities.cleanup === false)
const reclaimableLabel = computed(() => t(cleanupUnavailable.value ? 'common.reclaimableNoCleanup' : 'recycle.col.canFree'))
const reclaimableHint = computed(() => cleanupUnavailable.value && volume.value ? cleanupUnavailableText(volume.value) : t('recycle.col.canFree.hint'))
const mount = computed(() => volume.value?.mount.path ?? '')
const shares = computed<RecycleShare[]>(() => doc.value?.shares ?? [])
const reportInfo = computed(() => doc.value?.report_info ?? null)
/** Shares a newer share report answers instead of the volume report (SPEC 5.16). */
const newerShareCount = computed(() => sharesOnly.value ? 0 : shares.value.filter(share => share.report_info && share.report_info.job_id !== reportInfo.value?.job_id).length)
/** The saved reports covering the volume, for the report picker. */
const coveringReports = computed(() => mount.value ? reportsCovering(history.value, mount.value, mount.value) : [])
const showReportPicker = computed(() => !sharesOnly.value && !!reportInfo.value && (newerShareCount.value > 0 || chosenReport.value != null || coveringReports.value.length > 1))
/** The chip's scope: the whole volume, plus the newer share reports mixed in. */
const chipScope = computed(() => {
  if (sharesOnly.value || !reportInfo.value) return undefined
  if (newerShareCount.value) return t('report.scope.volumeAndShares', { count: newerShareCount.value })
  const scan = history.value?.scans.find(candidate => candidate.job_id === reportInfo.value?.job_id)
  // Only a whole-volume report answers a volume target.
  return scan ? scopeText(scan, mount.value) : t('report.scope.volume')
})
/** A share row's own newer report, in words; '' for a row the page's report answers. */
function shareScanned(share: RecycleShare) {
  const own = share.report_info
  return own && !sharesOnly.value && own.job_id !== reportInfo.value?.job_id ? t('report.row.shareScan', { time: formatRelative(own.scan_finished) }) : ''
}
const topWarnings = computed(() => (doc.value?.warnings ?? []).map(warning => warning.code))
const pageBusy = computed(() => error.value?.code === 'busy' || errorCodes.value.some(item => item.value === 'busy'))
const displayedErrorCodes = computed(() => errorCodes.value.length ? errorCodes.value : [{ set: 'api', value: error.value?.code ?? 'unknown' }])
const expandedDocument = computed(() => (expandedKey.value ? oldestDocuments[expandedKey.value] ?? null : null))
const expandedApiErrorCodes = computed(() => (expandedKey.value ? oldestRequestErrors[expandedKey.value] ?? [] : []))
const expandedCodes = computed<DisplayCode[]>(() => [
  ...(expandedDocument.value?.errors ?? []).map(item => ({ set: 'error', value: item.code })),
  ...(expandedDocument.value?.warnings ?? []).map(item => ({ set: 'warning', value: item.code })),
])
const expandedWarnings = computed(() => (expandedDocument.value?.warnings ?? []).map(warning => warning.code))
const expandedReportInfo = computed(() => expandedDocument.value?.report_info ?? null)
const expandedPartial = computed(() => reportCompleteness(expandedReportInfo.value, expandedDocument.value?.completion, !!expandedDocument.value?.errors.length).warn)
const expandedNoReport = computed(() =>
  expandedReportInfo.value == null &&
  (expandedDocument.value?.errors.length ?? 0) > 0 &&
  expandedDocument.value!.errors.every(item => item.code === 'no_report'),
)
const expandedMissing = computed(() => expandedDocument.value?.errors.some(item => item.code === 'not_found') === true)
const expandedBusy = computed(() =>
  expandedDocument.value?.completion === 'busy' || expandedCodes.value.some(item => item.value === 'busy') ||
  expandedApiErrorCodes.value.some(item => item.value === 'busy'),
)
const expandedFailure = computed(() =>
  expandedApiErrorCodes.value.length > 0 ||
  (!!expandedDocument.value && expandedDocument.value.completion === 'failed' && !expandedNoReport.value),
)
const expandedUnavailable = computed(() => (expandedKey.value ? oldestUnavailable[expandedKey.value] === true : false))
const expandedListReady = computed(() =>
  expandedKey.value != null && oldest[expandedKey.value] != null &&
  !expandedNoReport.value && !expandedBusy.value && !expandedFailure.value && !expandedUnavailable.value,
)
const expandedChanged = computed(() => expandedReportInfo.value?.consistency === 'changed-during-scan')
const expandedStale = computed(() => expandedReportInfo.value?.stale ?? false)
const expandedChangeText = computed(() =>
  joinParts((expandedReportInfo.value?.change_reasons ?? []).map(reason => code('change', reason.kind)), locale),
)
const expandedReportChip = computed(() => {
  const info = expandedReportInfo.value
  if (!info) return ''
  return expandedStale.value
    ? t('recycle.chip.stale', { age: formatRelative(info.scan_finished) })
    : t('recycle.oldest.report', { time: formatRelative(info.scan_finished), state: code('consistency', visibleConsistency(info) ?? 'partial') })
})

const pausedJob = computed(
  () =>
    jobs.list.find(
      job => (job.state === 'paused' || job.state === 'interrupted') && job.filesystems.some(fs => fs.mount.path === mount.value),
    ) ?? null,
)
const pausedPercent = computed(() => {
  const progress = pausedJob.value?.progress
  if (!progress?.basis) return null
  const items = progress.basis.items
  const completed = items == null ? progress.entries : progress.items
  const total = items ?? progress.basis.entries
  if (total <= 0) return null
  return Math.min(99, Math.max(0, Math.round((completed / total) * 100)))
})
const pausedText = computed(() => {
  const job = pausedJob.value
  if (!job) return ''
  return job.state === 'paused' && pausedPercent.value != null ? t('recycle.chip.paused', { percent: pausedPercent.value }) : code('state', job.state)
})
const completeness = computed(() => reportCompleteness(reportInfo.value, doc.value?.completion, !!doc.value?.errors.length))
const partial = computed(() => completeness.value.warn)
const changed = computed(() => reportInfo.value?.consistency === 'changed-during-scan')
const changeText = computed(() => joinParts((reportInfo.value?.change_reasons ?? []).map(r => code('change', r.kind)), locale))
const partialCodes = computed(() => [
  ...(doc.value?.errors ?? []).map(e => ({ set: 'error', value: e.code })),
  ...(doc.value?.warnings ?? []).map(w => ({ set: 'warning', value: w.code })),
])
/** The reasons behind a partial report, in the body of its warning Callout. */
const partialDetails = computed(() => [...partialCodes.value.map(item => code(item.set, item.value)), t('state.partial.body')])
const topWarningDetails = computed(() => topWarnings.value.map(warning => code('warning', warning)))
/** A partial report keeps its banner; its notes would only repeat it. */
const pageNotes = computed(() => partial.value ? [] : [
  ...reportNotes(reportInfo.value, volume.value?.shares ?? [], doc.value?.completion),
  ...(topWarnings.value.length ? [[t('recycle.warning.title'), ...topWarningDetails.value].join('\n')] : []),
])
const expandedPartialDetails = computed(() => [...expandedCodes.value.map(item => code(item.set, item.value)), t('state.partial.body')])
const expandedWarningDetails = computed(() => expandedWarnings.value.map(warning => code('warning', warning)))
const expandedNotes = computed(() => [
  ...(expandedReportInfo.value ? reportNotes(expandedReportInfo.value, volume.value?.shares ?? []).filter(note => note === t('common.consistency.systemAreasOnly')) : []),
  ...(expandedWarnings.value.length && !expandedPartial.value ? [[t('recycle.warning.title'), ...expandedWarningDetails.value].join('\n')] : []),
])
/** The app container's height bounds the sticky detail, as on Packages. */
const lastScanSeconds = computed(() => {
  const ms = doc.value?.scan_hints?.[0]?.last_scan?.elapsed_ms
  return ms == null ? null : ms / 1000
})

/** Oldest first when the user sorts by "moved"; a missing ctime stays last. */
const compareCtime = (a: unknown, b: unknown) =>
  (a == null ? Infinity : Date.parse(a as string)) - (b == null ? Infinity : Date.parse(b as string))

const shareColumns = computed<DataGridColumn[]>(() => [
  { key: 'share', label: t('recycle.col.share'), sortable: true, minWidth: 140 },
  { key: 'items', label: t('recycle.col.items'), align: 'right', sortable: true, width: '72px' },
  { key: 'bytes', label: t('recycle.col.allocated'), align: 'right', sortable: true, width: '96px' },
  { key: 'reclaimable', label: reclaimableLabel.value, align: 'right', sortable: true, width: '96px' },
])
const oldestColumns = computed<DataGridColumn[]>(() => [
  { key: 'path', sortable: true, label: t('recycle.oldest.col.path'), minWidth: 80 },
  { key: 'moved', label: t('recycle.oldest.moved'), sortable: true, compare: compareCtime, width: '140px' },
  { key: 'bytes', label: t('recycle.col.allocated'), align: 'right', sortable: true, width: '76px' },
  { key: 'reclaimable', label: reclaimableLabel.value, align: 'right', sortable: true, width: '76px' },
])

/** A shared folder's path under the volume ("share59/folder0149"); the full path stays its title. */
function shareName(path: string) {
  const prefix = `${mount.value?.replace(/\/$/, '')}/`
  return mount.value && path.startsWith(prefix) ? path.slice(prefix.length) : path
}

const shareRows = computed(() =>
  shares.value.map(share => ({
    key: share.share.path,
    share: shareName(share.share.path),
    items: share.items,
    bytes: share.bytes,
    reclaimable: share.reclaimable_bytes,
    owner: share,
  })),
)
const expandedShare = computed(() => shares.value.find(s => s.share.path === expandedKey.value) ?? null)
const oldestItems = computed(() => (expandedKey.value ? oldest[expandedKey.value] ?? [] : []))
const oldestLimit = computed(() => (expandedKey.value ? limits[expandedKey.value] ?? OLDEST_PAGE : OLDEST_PAGE))
const oldestRows = computed(() =>
  sortOldest(oldestItems.value).map(item => ({
    key: item.path.path,
    path: item.path.path,
    moved: item.ctime,
    bytes: item.bytes,
    reclaimable: item.reclaimable_bytes,
    owner: item,
  })),
)
const moreItems = computed(() => (expandedShare.value ? canLoadMore(expandedShare.value.items, oldestItems.value.length, oldestLimit.value) : false))
const cappedAtMax = computed(
  () => (expandedShare.value ? oldestItems.value.length < expandedShare.value.items && oldestLimit.value >= OLDEST_MAX : false),
)
/** What Reclaim moved into each shared folder's Recycle Bin since the scan; not in the sizes yet. */
const movedByReclaim = computed(() => doc.value?.moved_by_reclaim ?? [])
const movedTotal = computed(() => movedByReclaim.value.reduce((sum, moved) => sum + moved.bytes, 0))
const expandedMoved = computed(() => expandedShare.value ? movedByReclaim.value.find(moved => moved.share.path === expandedShare.value?.share.path) ?? null : null)

function reset() {
  loadRequestId += 1
  oldestRequestId += 1
  doc.value = null
  error.value = null
  errorCodes.value = []
  expandedKey.value = null
  oldestLoading.value = false
  copiedErrorMessage.value = ''
  for (const key of Object.keys(oldest)) delete oldest[key]
  for (const key of Object.keys(limits)) delete limits[key]
  for (const key of Object.keys(oldestDocuments)) delete oldestDocuments[key]
  for (const key of Object.keys(oldestRequestErrors)) delete oldestRequestErrors[key]
  for (const key of Object.keys(oldestApiErrors)) delete oldestApiErrors[key]
  for (const key of Object.keys(oldestUnavailable)) delete oldestUnavailable[key]
}

async function load() {
  const requestId = ++loadRequestId
  if (!volume.value) {
    status.value = 'ready'
    return
  }
  status.value = 'loading'
  error.value = null
  errorCodes.value = []
  copiedErrorMessage.value = ''
  sharesOnly.value = false
  sharesOnlyBase.value = null
  sharePaths.value = []
  shareAnswers.value = []
  try {
    const chosen = chosenReport.value
    const [result, saved] = await Promise.all([
      api('recycle.list', { target: volume.value.mount.path, ...(chosen ? { report: chosen } : {}) }),
      api('history.list', {}).catch(() => null),
    ])
    const noReport = result.report_info == null && result.errors.length > 0 && result.errors.every(item => item.code === 'no_report')
    let answers: (RecycleDocument | null)[] = []
    // A chosen report answers alone; otherwise each share answers from its newest report.
    sharePaths.value = chosen || !saved ? [] : newerScannedShares(saved, volume.value.mount.path, result.report_info?.scan_finished)
    answers = await loadShareAnswers(volume.value.mount.path, sharePaths.value.slice(0, SHARE_METRIC_PAGE))
    if (requestId !== loadRequestId) return
    history.value = saved
    sharesOnlyBase.value = result
    shareAnswers.value = answers
    sharesOnly.value = noReport && sharePaths.value.length > 0
    doc.value = answers.length ? shareAnswersAsVolume(result, answers) : result
    if (result.completion === 'busy' || (result.completion === 'failed' && !noReport)) {
      errorCodes.value = result.errors.length
        ? result.errors.map(item => ({ set: 'error', value: item.code }))
        : [{ set: 'error', value: result.completion === 'busy' ? 'busy' : 'unknown' }]
      status.value = 'error'
      return
    }
    status.value = 'ready'
  } catch (caught) {
    if (requestId !== loadRequestId) return
    error.value = caught instanceof ApiError ? caught : new ApiError('unknown')
    errorCodes.value = [{ set: 'api', value: error.value.code }]
    status.value = 'error'
  }
}

/** The recycle answers of one page of shares; a share that fails stays null. */
async function loadShareAnswers(mount: string, shares: Path[]) {
  return Promise.all(shares.map(share => api('recycle.list', { target: share.path_hex ? share : share.path }).catch(() => null)))
}

const remainingShareCount = computed(() => Math.max(0, sharePaths.value.length - shareAnswers.value.length))

/** Fetch the next page of share answers instead of every share up front (review §1). */
async function loadMoreShares() {
  const at = volume.value?.mount.path
  const base = sharesOnlyBase.value
  if (!at || !base) return
  const next = sharePaths.value.slice(shareAnswers.value.length, shareAnswers.value.length + SHARE_METRIC_PAGE)
  if (!next.length) return
  const answers = await loadShareAnswers(at, next)
  shareAnswers.value = [...shareAnswers.value, ...answers]
  doc.value = shareAnswersAsVolume(base, shareAnswers.value)
}

async function loadOldest(key: string, target: string, limit: number) {
  const requestId = ++oldestRequestId
  oldestLoading.value = true
  delete oldestDocuments[key]
  delete oldestRequestErrors[key]
  delete oldestApiErrors[key]
  delete oldestUnavailable[key]
  try {
    const result = await api('recycle.list', { target, oldest: limit, ...(chosenReport.value ? { report: chosenReport.value } : {}) })
    if (requestId !== oldestRequestId) return
    oldestDocuments[key] = result
    const noReport = result.report_info == null && result.errors.length > 0 && result.errors.every(item => item.code === 'no_report')
    if (result.completion === 'busy') {
      oldestRequestErrors[key] = result.errors.length
        ? result.errors.map(item => ({ set: 'error', value: item.code }))
        : [{ set: 'error', value: 'busy' }]
      return
    }
    if (result.completion === 'failed' && !noReport) {
      oldestRequestErrors[key] = result.errors.length
        ? result.errors.map(item => ({ set: 'error', value: item.code }))
        : [{ set: 'error', value: 'unknown' }]
      return
    }
    const share = result.shares.find(share => shareTarget(mount.value, share.share) === shareTarget(mount.value, { path: key }))
    if (share?.oldest == null && !noReport) {
      oldestUnavailable[key] = true
      return
    }
    oldest[key] = share?.oldest ?? []
    limits[key] = limit
  } catch (caught) {
    if (requestId !== oldestRequestId) return
    const apiError = caught instanceof ApiError ? caught : new ApiError('unknown')
    oldestApiErrors[key] = apiError
    oldestRequestErrors[key] = [{ set: 'api', value: apiError.code }]
  } finally {
    if (requestId === oldestRequestId) oldestLoading.value = false
  }
}

function toggle(share: RecycleShare) {
  const key = share.share.path
  oldestRequestId += 1
  oldestLoading.value = false
  if (expandedKey.value === key) {
    expandedKey.value = null
    return
  }
  expandedKey.value = key
  if (!oldest[key]) void loadOldest(key, recycleTarget(mount.value, share.share), OLDEST_PAGE)
}

function loadMore() {
  const share = expandedShare.value
  if (!share) return
  const key = share.share.path
  void loadOldest(key, recycleTarget(mount.value, share.share), nextOldestLimit(limits[key] ?? OLDEST_PAGE))
}

function retryOldest() {
  const share = expandedShare.value
  if (share) void loadOldest(share.share.path, recycleTarget(mount.value, share.share), oldestLimit.value)
}

async function copyErrorDetails() {
  const value = displayedErrorCodes.value[0]?.value ?? 'unknown'
  const detail = {
    code: value,
    exit: error.value?.exit ?? null,
    job_id: doc.value?.job_id ?? (typeof error.value?.detail?.job_id === 'string' ? error.value.detail.job_id : null),
  }
  const copied = await writeClipboard(JSON.stringify(detail))
  copiedErrorMessage.value = t(copied ? 'recycle.error.copied' : 'recycle.error.copyFailed')
}

async function copyOldestErrorDetails() {
  const value = expandedApiErrorCodes.value[0]?.value ?? expandedCodes.value[0]?.value ?? 'unknown'
  const document = expandedDocument.value as (RecycleDocument & { exit?: number }) | null
  const apiError = expandedKey.value ? oldestApiErrors[expandedKey.value] : undefined
  const detail = {
    code: value,
    exit: apiError?.exit ?? document?.exit ?? null,
    job_id: document?.job_id ?? null,
    target: document?.target ?? (expandedShare.value ? { path: recycleTarget(mount.value, expandedShare.value.share) } : null),
    errors: document?.errors ?? [],
    warnings: document?.warnings ?? [],
    ...(apiError?.detail ? { detail: apiError.detail } : {}),
  }
  const copied = await writeClipboard(JSON.stringify(detail))
  copiedErrorMessage.value = t(copied ? 'recycle.error.copied' : 'recycle.error.copyFailed')
}

/** One line per holder of a share's space: the first leads its cell. */
function heldLines(heldBy: RecycleShare['held_by']) {
  return heldBy.map(held => `${code('held_by', held.class)} ${formatBytes(held.bytes)}`)
}

/** An item's path inside its Recycle Bin; the cell's title keeps the full path. */
function inRecycleBin(path: string) {
  const at = path.indexOf('/#recycle/')
  return at < 0 ? path : path.slice(at + '/#recycle/'.length)
}

/** Opens the shared folder a Recycle Bin row names in File Station, through the same share check as Explore. */
function openShareFolder(share: RecycleShare) {
  openInFileStation({ path: shareTarget(mount.value, share.share), kind: 'dir' })
}

/**
 * Gives the ListDetail pane this element is in a test id: the acceptance suite
 * finds the open share's pane, its heading and its oldest items by it.
 */
const vPaneTestId: Directive<HTMLElement, string> = {
  mounted(element, binding) { element.closest<HTMLElement>('[data-ui-list-detail-pane]')?.setAttribute('data-testid', binding.value) },
  beforeUnmount(element) { element.closest<HTMLElement>('[data-ui-list-detail-pane]')?.removeAttribute('data-testid') },
}

watch(mount, () => {
  chosenReport.value = null
  reset()
  void load()
}, { immediate: true })
watch(chosenReport, () => {
  reset()
  void load()
})
</script>

<template>
  <div class="recycle-page">
    <template v-if="pausedJob">
      <PageStatus :label="pausedText" tone="warning" />
      <PageActions>
        <Button variant="ghost" size="sm" @click="navigate('scanning')">{{ t('recycle.chip.viewJob') }}</Button>
      </PageActions>
    </template>
    <ScanBadge v-else-if="reportInfo" :report="reportInfo" :scope="chipScope" :consistency="partial ? 'partial' : undefined" :details="changed ? changeText : undefined" />

    <div v-if="status === 'loading'" class="recycle-loading">
      <LoadingArea :text="t('common.loading')" />
    </div>

    <StateView v-else-if="status === 'error'" :kind="pageBusy ? 'busy' : 'error'" :codes="displayedErrorCodes">
      <Button v-if="pageBusy" variant="ghost" @click="load">{{ t('common.retry') }}</Button>
      <Button variant="ghost" @click="copyErrorDetails">{{ t('common.copyDetails') }}</Button>
      <p v-if="copiedErrorMessage" role="status">{{ copiedErrorMessage }}</p>
    </StateView>

    <StateView v-else-if="!reportInfo && !sharesOnly" kind="no_report" :last-scan-seconds="lastScanSeconds">
      <Button variant="primary" @click="openNewScan">{{ t('recycle.scanNow') }}</Button>
    </StateView>

    <template v-else>
      <Callout v-if="sharesOnly" :dismissible="false" :title="t('recycle.sharesOnly.title')">
        {{ t('recycle.sharesOnly.body') }}
        <template #actions>
          <Button v-if="remainingShareCount" size="sm" variant="ghost" @click="loadMoreShares">
            {{ t('recycle.sharesOnly.loadMore', { count: remainingShareCount }) }}
          </Button>
        </template>
      </Callout>
      <ReportPicker
        v-if="showReportPicker"
        v-model="chosenReport"
        class="recycle-report"
        data-testid="recycle-report"
        :reports="coveringReports"
        :mount="mount"
        :mixed="newerShareCount > 0"
      />
      <PartialHint v-if="partial" :details="partialDetails" />
      <ScanNotes :notes="pageNotes" />
      <Callout tone="info" :title="t('recycle.hint.title')" data-test="recycle-dsm">
        <p>{{ t('recycle.hint.dsm') }}</p>
        <p v-if="movedTotal" data-test="recycle-moved">{{ t('recycle.moved.total', { bytes: formatBytes(movedTotal), count: movedByReclaim.length }) }}</p>
      </Callout>
      <Button v-if="!sharesOnly && remainingShareCount" size="sm" @click="loadMoreShares">{{ t('recycle.sharesOnly.loadMore', { count: remainingShareCount }) }}</Button>
      <div v-if="!shareRows.length">
        <EmptyState v-if="sharesOnly" :title="t('recycle.sharesOnly.emptyTitle')" />
        <EmptyState v-else :title="t('recycle.empty.title')" :description="t('recycle.empty.body')" />
      </div>

      <ListDetail
        v-else
        storage-key="recycle"
        :title="expandedShare ? shareName(expandedShare.share.path) : undefined"
        @close="expandedKey = null"
      >
        <ReclaimGrid
          table-id="recycle-shares-v2"
          :columns="shareColumns"
          :rows="shareRows"
          row-key="key"
          :selected-keys="expandedKey ? [expandedKey] : []"
          :empty-label="t('recycle.empty.title')"
        >
          <template #cell-share="{ row }">
            <button
              type="button"
              class="recycle-share-button"
              :aria-expanded="expandedKey === row.key"
              :aria-label="t(expandedKey === row.key ? 'recycle.oldest.hideFor' : 'recycle.oldest.showFor', { share: row.key as string })"
              @click.stop="toggle(row.owner as RecycleShare)"
            >
              <CellText :data-ui-tooltip-text="String(row.key)" :text="String(row.key)" :secondary="shareScanned(row.owner as RecycleShare) || undefined">
                <span class="text-[var(--ui-primary)]">{{ row.share }}</span>
              </CellText>
            </button>
          </template>
          <template #cell-items="{ row }">{{ formatCount(row.items as number) }}</template>
          <template #cell-bytes="{ row }">{{ formatBytes(row.bytes as number) }}</template>
          <template #header-bytes>
            <Tooltip :content="t('recycle.col.allocated.hint')">
              <span class="recycle-hint-label">{{ t('recycle.col.allocated') }}</span>
            </Tooltip>
          </template>
          <template #cell-reclaimable="{ row }">{{ formatBytes(row.reclaimable as number | null) }}</template>
          <template #header-reclaimable>
            <Tooltip :content="reclaimableHint">
              <span class="recycle-hint-label">{{ reclaimableLabel }}</span>
            </Tooltip>
          </template>
        </ReclaimGrid>

        <template v-if="expandedShare" #detail>
          <span v-pane-test-id="'recycle-detail'" hidden />
          <Space wrap :spacing="8" class="recycle-detail-meta">
            <Button
              variant="ghost"
              size="sm"
              :aria-label="t('recycle.openInFileStation', { share: expandedShare.share.path })"
              @click="openShareFolder(expandedShare)"
            >
              <FolderOpen class="size-4" />{{ t('recycle.detail.openFolder') }}
            </Button>
            <Tooltip v-if="expandedChanged" :content="expandedChangeText">
              <Badge :tone="expandedStale ? 'warning' : 'neutral'">{{ expandedReportChip }}</Badge>
            </Tooltip>
            <Badge v-else-if="expandedReportInfo" :tone="expandedStale ? 'warning' : 'neutral'">{{ expandedReportChip }}</Badge>
          </Space>
          <p v-if="heldLines(expandedShare.held_by).length" class="recycle-held">
            <Tooltip :content="t('recycle.col.heldBy.hint')">
              <span class="recycle-hint-label" tabindex="0">{{ t('recycle.col.heldBy') }}</span>
            </Tooltip>
            {{ heldLines(expandedShare.held_by).join(' · ') }}
          </p>
          <p v-if="expandedMoved" class="recycle-held" data-test="recycle-share-moved">{{ t('recycle.moved.share', { bytes: formatBytes(expandedMoved.bytes), count: expandedMoved.items }) }}</p>

          <h3 class="recycle-oldest-title">{{ t('recycle.detail.oldest') }}</h3>
          <LoadingArea v-if="oldestLoading" :text="t('recycle.oldest.loading')" />
          <StateView v-else-if="expandedNoReport" kind="no_report">
            <Button variant="primary" @click="openNewScan">{{ t('recycle.scanNow') }}</Button>
          </StateView>
          <Callout v-else-if="expandedMissing" :dismissible="false" tone="warning" :title="t('recycle.oldest.missingTitle')">
            {{ t('recycle.oldest.missingBody') }}
            <template #actions>
              <Button @click="openNewScan">{{ t('recycle.scanNow') }}</Button>
              <Button variant="ghost" @click="copyOldestErrorDetails">{{ t('common.copyDetails') }}</Button>
              <p v-if="copiedErrorMessage" role="status">{{ copiedErrorMessage }}</p>
            </template>
          </Callout>
          <StateView v-else-if="expandedBusy || expandedFailure" :kind="expandedBusy ? 'busy' : 'error'" :codes="expandedApiErrorCodes.length ? expandedApiErrorCodes : expandedCodes">
            <Button v-if="expandedBusy" variant="ghost" @click="retryOldest">{{ t('common.retry') }}</Button>
            <Button variant="ghost" @click="copyOldestErrorDetails">{{ t('common.copyDetails') }}</Button>
            <p v-if="copiedErrorMessage" role="status">{{ copiedErrorMessage }}</p>
          </StateView>
          <Callout v-else-if="expandedUnavailable" :dismissible="false" tone="warning">
            {{ t('recycle.oldest.unavailable') }}
            <template #actions>
              <Button variant="ghost" @click="retryOldest">{{ t('common.retry') }}</Button>
            </template>
          </Callout>
          <PartialHint v-else-if="expandedPartial" :details="expandedPartialDetails" />
          <ScanNotes :notes="expandedNotes" />
          <Space v-if="expandedListReady" wrap :spacing="8" justify-content="space-between" class="recycle-oldest-actions">
            <div class="recycle-oldest-count">
              <span v-if="moreItems">{{ t('recycle.oldest.firstOnly', { count: oldestRows.length, total: expandedShare.items }) }}</span>
              <span v-else-if="cappedAtMax">{{ t('recycle.oldest.maxOnly', { count: oldestRows.length }) }}</span>
              <Button v-if="moreItems" variant="ghost" size="sm" @click="loadMore">{{ t('recycle.oldest.loadMore') }}</Button>
            </div>
          </Space>
          <ReclaimGrid
            v-if="expandedListReady"
            table-id="recycle-oldest-v2"
            :fill="false"
            :columns="oldestColumns"
            :rows="oldestRows"
            row-key="key"
            :empty-label="t('recycle.oldest.empty')"
          >
            <template #header-moved>
              <Tooltip :content="t('recycle.oldest.moved.hint')">
                <span class="recycle-hint-label">{{ t('recycle.oldest.moved') }}</span>
              </Tooltip>
            </template>
            <template #header-bytes>
              <Tooltip :content="t('recycle.col.allocated.hint')">
                <span class="recycle-hint-label">{{ t('recycle.col.allocated') }}</span>
              </Tooltip>
            </template>
            <template #header-reclaimable>
              <Tooltip :content="reclaimableHint">
                <span class="recycle-hint-label">{{ reclaimableLabel }}</span>
              </Tooltip>
            </template>
            <template #cell-path="{ row }">
              <CellText class="font-[family-name:var(--ui-font-mono)]" :data-ui-tooltip-text="String(row.path)" :text="String(row.path)">{{ inRecycleBin(String(row.path)) }}</CellText>
            </template>
            <template #cell-moved="{ row }">
              <CellText v-if="row.moved" :text="formatCompactDateTime(row.moved as string)" :title="formatDateTime(row.moved as string)" />
              <span v-else class="recycle-unknown">{{ t('common.unknown') }}</span>
            </template>
            <template #cell-bytes="{ row }">{{ formatBytes(row.bytes as number) }}</template>
            <template #cell-reclaimable="{ row }">{{ formatBytes(row.reclaimable as number | null) }}</template>
          </ReclaimGrid>
        </template>
      </ListDetail>
    </template>
  </div>
</template>

<style scoped>
.recycle-page {
  display: flex;
  height: 100%;
  min-height: 0;
  flex-direction: column;
  gap: var(--ui-section-gap);
}
.recycle-loading {
  display: flex;
  justify-content: center;
  padding: 48px 0;
}
.recycle-share-button {
  display: flex;
  width: 100%;
  min-width: 0;
  border-radius: var(--ui-radius-md);
  text-align: left;
  outline: none;
}
.recycle-share-button:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
.recycle-oldest-title {
  font-size: var(--ui-font-size);
  font-weight: 600;
  color: var(--ui-text);
}
.recycle-held {
  margin: 0;
  color: var(--ui-text);
}
.recycle-oldest-count {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: var(--ui-font-size);
  color: var(--ui-text-secondary);
}
.recycle-unknown {
  color: var(--ui-text-muted);
}
.recycle-hint-label {
  text-decoration: underline dotted;
  text-underline-offset: 2px;
}
</style>
