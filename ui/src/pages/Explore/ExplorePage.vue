<script setup lang="ts">
import ScanBadge from '@/components/ScanBadge.vue'
import { PageInfo } from '@synopackageland/sdk/vue'
import { reportCompleteness } from '@/components/reportCompleteness'
import { approximationNote } from '@/components/approximation'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { computed, ref, watch } from 'vue'
import { readLocal } from '@/stores/storage'
import { Breadcrumb, Button, Callout, DropdownMenu, EmptyState, InfoTip, SearchField, SectionTabs, Sheet, SplitPane, StorageMeter, LoadingMask, useToast } from '@synopackageland/ui'
import type { BreadcrumbItem, DataGridColumn, DataGridSort, DropdownMenuEntry } from '@synopackageland/ui'
import { File, Folder, FolderOpen, Lightbulb, Settings2 } from '@lucide/vue'
import { ApiError, api } from '@/api'
import BasketBar from '@/components/BasketBar.vue'
import SystemAreasNote from '@/components/SystemAreasNote.vue'
import { heldByOfRoot, newerScannedShares, nextMetricTargets, oldestReport, scannedShares } from '@/components/shareReports'
import ReportPicker from '@/components/ReportPicker.vue'
import { reportsCovering, scopeText } from '@/components/reportChoice'
import { useElementSize } from '@/composables/useElementSize'
import StateView from '@/components/states/StateView.vue'
import type { StateKind } from '@/components/states/states'
import { LABEL_SEPARATOR, code, formatBytes, formatCount, formatRelative, formatVolume, joinParts, locale, t } from '@/i18n'
import type { HistoryDocument, Path, TopDocument } from '@/types'
import { ReviewDialog } from '@/pages/registry'
import { useBasket } from '@/stores/basket'
import { answersVersion } from '@/stores/answers'
import VolumeCapabilities from '@/components/VolumeCapabilities.vue'
import { selectedVolume, volumeList, volumes } from '@/stores/volumes'
import { openNewScan, openPackage, takeExploreRequest } from '@/stores/nav'
import { crumbChain, crumbName } from '@/components/crumbs'
import VolumePicker from '@/components/VolumePicker.vue'
import { cleanupUnavailableText } from '@/components/cleanupUnavailable'
import { fileStationBlock, fileStationBlockKey, openInFileStation } from '@/components/fileStation'
import { entryNotes, protectedKind } from './entryNotes'
import Treemap from './Treemap.vue'
import ExploreCell from './ExploreCell.vue'
import type { ExploreCellRow } from './ExploreCell.vue'
import { EXPLORE_SPLIT, LEGEND_INLINE_MIN, exploreSplitDefault, EXPLORE_SPLIT_GAP, NAME_FULL_MIN, exploreStacked, listColumns } from './exploreLayout'
import { COMPACT_BASKET_BELOW, gridRoom, headerWidth } from '@/components/listLayout'
import { usageBaselineMissing } from '@/usage/useUsageChange'
import { scrollbarWidth, textMeasure } from './measureText'
import { entryDisplayName, serviceName, sizedEntries } from './treemap'
import type { ExploreEntry } from './treemap'
import { useBasketEstimate } from '@/composables/useBasketEstimate'

type BrowseShareEntry = {
  browseOnly: true
  path: string
  path_hex?: string
  kind: 'dir'
  flags: string[]
  /** A share report's root totals; null without a report of the share. */
  metrics: { physical: number | null; reclaimable: number | null }
  /** When the share's own newer report finished, for a share it answers at a volume report's root. */
  scanned?: string
  held_by: ExploreEntry['held_by'] | null
  service: null
  cleanup_eligible: false
}

type TopResult = TopDocument['results'][number]

/** A share scanned on its own, and its report's answer at the share root (null when it failed). */
type ShareReport = { path: Path; root: TopResult | null }

type ExploreRowEntry = ExploreEntry | BrowseShareEntry

const view = ref<'all' | 'dirs' | 'files'>('all')
/** The CLI's top-list maximum; the page asks for and shows no more (review §1 P1). */
const TOP_LIMIT = 200
const search = ref('')
/** The target the latest navigation asked for; the crumb list is derived from it. */
const requestedTarget = ref<Path | null>(null)
const document = ref<TopDocument | null>(null)
const requestError = ref<ApiError | null>(null)
const loading = ref(false)
const requestSerial = ref(0)
/**
 * The navigation the screen is waiting for. A new level aborts the previous
 * one, so a superseded query is dropped from the DSM queue instead of taking
 * one of its two slots (design review 2026-10-01 §7.3).
 */
let navigation: AbortController | null = null
const sort = ref<DataGridSort | null>({ key: 'reclaimable', direction: 'desc' })
const reviewOpen = ref(false)
const detailOpen = ref(false)
const detailEntry = ref<ExploreEntry | null>(null)
/**
 * Shares with a report of their own: every one when the volume has no
 * whole-volume report, else those scanned after it (the newest report of
 * each path answers it).
 */
const shareReports = ref<ShareReport[]>([])
/** The volume's saved scans: which reports can answer, and their scope. Read once per volume and answer version. */
const history = ref<HistoryDocument | null>(null)
let historyRead: { key: string; answer: Promise<HistoryDocument | null> } | null = null
/** The saved report the user chose by its job ID; null answers from the newest report of each path. */
const chosenReport = ref<string | null>(null)
/** The target of the answer on screen; it lags `currentTarget` while a level loads. */
const shownTarget = ref<Path | null>(null)
/** Share reports whose root totals were already asked for (a failure is not retried). */
const metricsAttempted = ref(new Set<string>())

const currentVolume = computed(() => selectedVolume.value)
// A volume this app cannot clean is still explored; nothing on it can be selected.
const cleanupUnavailable = computed(() => currentVolume.value?.capabilities.cleanup === false)
const currentTarget = computed(() => requestedTarget.value ?? currentVolume.value?.mount ?? null)
/**
 * The crumb list, derived from the target of the current navigation: a superseded
 * navigation is replaced, never appended, so it cannot leave a crumb behind.
 */
const crumbs = computed(() => crumbChain(currentVolume.value?.mount ?? null, currentTarget.value))
const breadcrumbItems = computed<BreadcrumbItem[]>(() => crumbs.value.map((crumb, index) => ({ display: crumbName(crumb.path), id: String(index) })))
const basket = computed(() => currentVolume.value ? useBasket(currentVolume.value.mount.path) : null)
// The grid asks each row's key several times per draw (selectable, reason,
// label), so a path's hex is encoded once, not on every ask.
const pathHex = new Map<string, string>()

const basketItems = computed(() => basket.value?.items.value ?? [])
const basketCount = computed(() => basket.value?.count.value ?? 0)
const basketLowerBound = computed(() => basket.value?.lowerBound.value ?? 0)
const basketSnapshotHeldBytes = computed(() => basket.value?.snapshotHeldBytes.value ?? 0)
const basketUnknownCount = computed(() => basket.value?.unknownCount.value ?? 0)
const basketSummaryDetails = computed(() => {
  const details = [
    ...(basketSnapshotHeldBytes.value > 0
      ? [t('explore.basket.summaryHeld', { bytes: formatBytes(basketSnapshotHeldBytes.value) })]
      : []),
    ...(basketUnknownCount.value > 0
      ? [t('explore.basket.unknown', { count: basketUnknownCount.value })]
      : []),
  ]
  return details.length
    ? t('explore.basket.summaryDetails', {
        details: details.join(t('explore.basket.summarySeparator')),
      })
    : ''
})
const basketKeys = computed(() => basketItems.value.map(pathKey))
const basketKeySet = computed(() => new Set(basketKeys.value))
// The exact amount for the whole set; the lower bound stands in until it answers.
const basketEstimate = useBasketEstimate(() => basketItems.value)
// Duplicate groups from the Duplicates page add their own estimate; simulate.get covers paths only.
const duplicateGroups = computed(() => basket.value?.duplicates.value.groups ?? [])
const basketBytes = computed(() => basketEstimate.exact.value == null
  ? basketLowerBound.value
  : basketEstimate.exact.value + duplicateGroups.value.reduce((sum, group) => sum + group.canFree, 0))
const basketExact = computed(() => basketEstimate.exact.value != null && !duplicateGroups.value.length)
const volumeSpace = computed(() => {
  const volume = currentVolume.value
  return volume?.total_bytes != null && volume.free_bytes != null ? { total: volume.total_bytes, free: volume.free_bytes } : null
})
const toast = useToast()
const report = computed(() => document.value?.results[0]?.report ?? null)
const entries = computed(() => document.value?.results[0]?.entries ?? [])
/** Whether the level on screen is the volume root; the previous level keeps its rows while the next loads. */
const shownAtVolumeRoot = computed(() => Boolean(
  shownTarget.value && currentVolume.value && pathKey(shownTarget.value) === pathKey(currentVolume.value.mount),
))
const noReport = computed(() => document.value?.errors.some(error => error.code === 'no_report') ?? false)
/** No whole-volume report: the first level lists the shares scanned on their own. */
const sharesOnly = computed(() => noReport.value && shareReports.value.length > 0)
/**
 * Shares a newer share report answers at a volume report's root: only once
 * that share's own answer came from a report other than the volume's.
 */
const newerShares = computed(() => {
  const volumeJob = report.value?.job_id
  if (sharesOnly.value || !volumeJob) return []
  return shareReports.value.filter(share => share.root && share.root.report.job_id !== volumeJob)
})
const newerShareKeys = computed(() => new Set(newerShares.value.map(share => pathKey(share.path))))
const browseShares = computed<BrowseShareEntry[]>(() => {
  if (!shownAtVolumeRoot.value || view.value !== 'all') return []
  const shareKeys = new Set((currentVolume.value?.shares ?? []).map(share => pathKey(share.path)))
  if (sharesOnly.value) {
    return shareReports.value.map(report => browseEntry(report.path, report.root, shareKeys.has(pathKey(report.path))))
  }
  const pathsInResult = new Set(entries.value.map(pathKey))
  const newer = newerShares.value.map(share => ({ ...browseEntry(share.path, share.root, shareKeys.has(pathKey(share.path))), scanned: share.root!.report.scan_finished }))
  return [
    ...newer,
    ...(currentVolume.value?.shares ?? [])
      .filter(share => !pathsInResult.has(pathKey(share.path)) && !newerShareKeys.value.has(pathKey(share.path)))
      .map(share => browseEntry(share.path)),
  ]
})
/** The volume report's rows, less the shares a newer share report answers, then the browse rows. */
const visibleRows = computed<ExploreRowEntry[]>(() => [
  ...(newerShareKeys.value.size && shownAtVolumeRoot.value ? entries.value.filter(entry => !newerShareKeys.value.has(pathKey(entry))) : entries.value),
  ...browseShares.value,
])
/** The saved reports covering the level on screen, for the report picker. */
const coveringReports = computed(() => {
  const mount = currentVolume.value?.mount.path
  const shown = shownTarget.value
  return mount && shown ? reportsCovering(history.value, mount, shown.path) : []
})
/** The picker shows when there is a choice: another report, or newer share reports mixed in. */
const showReportPicker = computed(() => !sharesOnly.value && (coveringReports.value.length > 1 || newerShares.value.length > 0 || chosenReport.value != null))
/** The chip's scope: what the answer's report covers, plus the newer share reports mixed in. */
const chipScope = computed(() => {
  const mount = currentVolume.value?.mount.path ?? ''
  if (newerShares.value.length) return t('report.scope.volumeAndShares', { count: newerShares.value.length })
  const job = report.value?.job_id
  const scan = history.value?.scans.find(candidate => candidate.job_id === job)
    ?? history.value?.scans.find(candidate => candidate.job_id === chosenReport.value)
  if (scan) return scopeText(scan, mount)
  // Only a whole-volume report answers at the volume root.
  return report.value && shownAtVolumeRoot.value ? t('report.scope.volume') : undefined
})
/** Beside the search bar: what the search covers, and why some shares show no size. */
const pageNotes = computed(() => [
  // The shell leaves this hint to Explore's info button, so the list starts higher.
  usageBaselineMissing.value && selectedVolume.value
    ? t('usageChange.volume', { volume: formatVolume(selectedVolume.value.mount.path), message: t('usageChange.noBaseline') })
    : '',
  t('explore.search.loadedOnly'),
  !sharesOnly.value && browseShares.value.some(entry => entry.metrics.physical == null) ? t('explore.state.shareMetricsMissing') : '',
])
const pendingShareMetrics = computed(() =>
  nextMetricTargets(
    shareReports.value.filter(entry => !entry.root).map(entry => entry.path.path),
    path => metricsAttempted.value.has(path),
  ),
)
// Worth a look: from what the scan knows about this level, never from
// names. Each reason says where the space can be dealt with.
const WORTH_LIMIT = 6
const worthALook = computed(() => {
  const candidates = entries.value.filter(entry => !entry.within)
  const levelBytes = candidates.reduce((sum, entry) => sum + (entry.metrics.physical ?? 0), 0)
  const floor = Math.max(1024 * 1024, levelBytes * 0.01)
  return candidates.flatMap(entry => {
    const physical = entry.metrics.physical ?? 0
    const reclaimable = entry.metrics.reclaimable ?? 0
    const snapshot = heldBytes(entry, 'snapshot')
    if (entry.flags.includes('recycle') && reclaimable >= floor)
      return [{ entry, score: reclaimable, reason: t('explore.worth.recycle', { bytes: formatBytes(reclaimable) }) }]
    if (entry.service && physical >= floor)
      return [{ entry, score: physical / 2, reason: t('explore.worth.service', { bytes: formatBytes(physical) }) }]
    if (physical > 0 && snapshot >= physical / 2 && snapshot >= floor)
      return [{ entry, score: snapshot / 2, reason: t('explore.worth.snapshot', { bytes: formatBytes(snapshot) }) }]
    if (entry.cleanup_eligible && reclaimable >= floor)
      return [{ entry, score: reclaimable, reason: t('explore.worth.reclaimable', { bytes: formatBytes(reclaimable) }) }]
    return []
  }).sort((left, right) => right.score - left.score).slice(0, WORTH_LIMIT)
})
const worthItems = computed<DropdownMenuEntry[]>(() => worthALook.value.length
  ? worthALook.value.map(item => ({ key: pathKey(item.entry), label: t('explore.worth.item', { name: entryDisplayName(item.entry), reason: item.reason }) }))
  : [{ key: '', label: t('explore.worth.empty'), disabled: true }])

function openWorth(key: string) {
  const item = worthALook.value.find(candidate => pathKey(candidate.entry) === key)
  if (item) onActivate(item.entry)
}

const lastScanSeconds = computed(() => {
  const elapsed = document.value?.scan_hints?.[0]?.last_scan?.elapsed_ms
  return elapsed == null ? null : elapsed / 1000
})

const volumeChoices = computed(() => volumeList.value.map(volume => ({
  value: volume.mount.path,
  label: pathName(volume.mount.path),
})))
const viewTabs = computed(() => [
  { value: 'all', label: t('explore.view.level') },
  { value: 'dirs', label: t('explore.view.folders') },
  { value: 'files', label: t('explore.view.files') },
])

const completeness = computed(() => reportCompleteness(report.value, document.value?.completion, !!document.value?.errors.length))
/** The reasons shown with a blocking state: the refused call, else the report errors. */
const stateCodes = computed(() => requestError.value ? [] : (document.value?.errors ?? []).map(error => ({ set: 'error', value: error.code })))
const canRetry = computed(() => stateKind.value === 'busy' || stateKind.value === 'error')
const stateKind = computed<StateKind | null>(() => {
  if (requestError.value) {
    if (requestError.value.code === 'no_permission') return 'not_admin'
    if (requestError.value.code === 'backend_unavailable') return 'backend_unavailable'
    if (requestError.value.code === 'busy') return 'busy'
    return 'error'
  }
  if (!document.value) return null
  if (noReport.value) return sharesOnly.value ? null : 'no_report'
  if (document.value.completion === 'busy') return 'busy'
  if (document.value.completion === 'failed') return 'error'
  if (
    completeness.value.warn
    || entries.value.some(entry => entry.metrics.physical == null)
  ) return 'partial'
  return null
})


const filteredRows = computed(() => {
  const query = search.value.trim().toLocaleLowerCase()
  if (!query) return visibleRows.value
  return visibleRows.value.filter(entry => entry.path.toLocaleLowerCase().includes(query))
})

const hasTreemapTiles = computed(() => sizedEntries(filteredRows.value).length > 0)
/** The report the toolbar chip describes: this answer's, else the oldest share report shown. */
const chipReport = computed(() => report.value ?? (sharesOnly.value ? oldestReport(shareReports.value.flatMap(share => share.root ? [share.root.report] : [])) : null))

// Only successful/partial read answers get compact coverage details. Interrupted
// scans and report errors retain their prominent StateView warning.
const compactPartial = computed(() => stateKind.value === 'partial'
  && ['complete', 'partial'].includes(document.value?.completion ?? '')
  && !document.value?.errors.length)
/** A state with nothing to list stands alone, without an empty map and table under it. */
const stateOnly = computed(() => !!stateKind.value && stateKind.value !== 'partial' && !visibleRows.value.length)
const coverageDetails = computed(() => {
  if (compactPartial.value) return [t('state.partial.title'), t('state.partial.body')].join('\n')
  if (!stateKind.value && report.value?.attribution?.approximation) return approximationNote(report.value)
  if (!stateKind.value && completeness.value.unknownNote)
    return t('state.unattributed.note', { bytes: formatBytes(completeness.value.unknownBytes) })
  return ''
})
/** Unread snapshots concern every row of the report, so they are said once here, not per row. */
const statusDetails = computed(() => {
  const unread = report.value?.coverage.snapshots_not_materialized ?? 0
  return [coverageDetails.value, unread > 0 ? t('explore.snapshotsNotRead', { count: unread }) : ''].filter(Boolean).join('\n')
})

/** The bars in the Size column share one scale: the largest row shown. */
const largestPhysical = computed(() => Math.max(0, ...filteredRows.value.map(entry => entry.metrics.physical ?? 0)))
type ExploreGridRow = ExploreCellRow & { entry: ExploreRowEntry }
/**
 * Each row's cells, worked out once when the list changes. A cell takes only
 * these values, so drawing the list again (a moved split, a resized window)
 * draws none of its cells again.
 */
const gridRows = computed(() => filteredRows.value.map((entry): ExploreGridRow => {
  const held = heldLabel(entry)
  const locationLines = placeLines(entry)
  const context = rowContext(entry)
  const holds = entry.held_by?.some(item => item.bytes > 0) ? held : ''
  return {
    id: pathKey(entry),
    path: entry.path,
    name: entryDisplayName(entry),
    icon: entryIcon(entry),
    context,
    compactContext: joinParts([context, holds, locationLines.place].filter(Boolean), locale),
    fileStation: fileStationReason(entry),
    physical: entry.metrics.physical,
    reclaimable: entry.metrics.reclaimable,
    held,
    location: locationLabel(entry),
    locationLines,
    entry,
  }
}))

// Size and Can free are fixed, wider where a language's header needs it
// (headerWidth). The name, Held by and Where to manage share what is left of
// the grid besides its vertical scrollbar (gridRoom); a narrow list keeps the
// name alone and says who holds the space and where to manage it under it
// (listColumns). Dragging the pane wider brings the columns back.
const gridHost = ref<HTMLElement | null>(null)
const gridSize = useElementSize(gridHost, { width: Infinity, height: 0 })

// The treemap moves above the table when the content area itself is narrow.
const layoutHost = ref<HTMLElement | null>(null)
const layoutSize = useElementSize(layoutHost, { width: Infinity, height: 0 })
const stackedLayout = computed(() => exploreStacked(layoutSize.value.width))
// The colour key moves behind an icon when stacked or when the map is narrow.
const mapPanel = ref<HTMLElement | null>(null)
const mapPanelSize = useElementSize(mapPanel, { width: Infinity, height: 0 })
const legendCollapsed = computed(() => stackedLayout.value || mapPanelSize.value.width < LEGEND_INLINE_MIN)
// A short or narrow window keeps the cleanup bar to one row, so the list keeps its rows.
const pageHost = ref<HTMLElement | null>(null)
const pageSize = useElementSize(pageHost, { width: Infinity, height: Infinity })
const compactBasket = computed(() => pageSize.value.height < COMPACT_BASKET_BELOW.height || pageSize.value.width < COMPACT_BASKET_BELOW.width)
/** The whole cleanup summary, for the compact bar's tooltip. */
const basketSummaryTitle = computed(() => [
  t(basketExact.value ? 'explore.basket.summaryLeadExact' : 'explore.basket.summaryLead', { count: basketCount.value }),
  ' ', formatBytes(basketBytes.value), t('explore.basket.summaryAfter'),
  basketSummaryDetails.value ? ` ${basketSummaryDetails.value}` : '',
].join(''))
// The share is remembered per orientation in this browser.
const splitOrientation = computed(() => stackedLayout.value ? 'vertical' : 'horizontal')
const split = computed(() => EXPLORE_SPLIT[splitOrientation.value])
// The share the user dragged to is kept by the SplitPane under this key; until
// then the treemap's share follows the width, so a maximized 1440 px window
// starts with the list's columns in full (B14).
const SPLIT_KEY = 'synoreclaim.explore.split'
const savedSplit = ref<Record<'horizontal' | 'vertical', number | null>>({ horizontal: savedShare('horizontal'), vertical: savedShare('vertical') })
const splitDefault = computed(() => exploreSplitDefault(splitOrientation.value, layoutSize.value.width))
const splitShare = computed(() => savedSplit.value[splitOrientation.value] ?? splitDefault.value)

function savedShare(orientation: 'horizontal' | 'vertical') {
  const share = readLocal<unknown>(`${SPLIT_KEY}:${orientation}`, null)
  return typeof share === 'number' && share > 0 && share < 1 ? share : null
}

/**
 * A drag or a key keeps its share; a double click clears the saved one, and
 * the default follows the width again. The SplitPane stores a dragged share
 * only on release, so the share is never read back from storage mid-drag: that
 * put the pane back where it started after every move and laid the whole page
 * out twice per move. A reset clears storage before it reports the default.
 */
function changeSplit(share: number) {
  const orientation = splitOrientation.value
  const reset = share === splitDefault.value && savedShare(orientation) == null
  const next = reset ? null : share
  if (savedSplit.value[orientation] === next) return
  savedSplit.value = { ...savedSplit.value, [orientation]: next }
}
const legendItems = computed(() => [
  { key: 'user', swatch: 'explore-legend-swatch', color: 'var(--ui-blue-9)', label: t('explore.treemap.legend.user') },
  { key: 'service', swatch: 'explore-legend-swatch', color: 'var(--ui-green-9)', label: t('explore.treemap.legend.service') },
  { key: 'recycle', swatch: 'explore-legend-swatch', color: 'var(--ui-orange-9)', label: t('explore.treemap.legend.recycle') },
  { key: 'system', swatch: 'explore-legend-swatch', color: 'var(--ui-gray-11)', label: t('explore.treemap.legend.system') },
  { key: 'folder', swatch: 'explore-legend-swatch explore-legend-swatch--folder', color: null, label: t('explore.treemap.legend.folder') },
  { key: 'file', swatch: 'explore-legend-swatch explore-legend-swatch--file', color: null, label: t('explore.treemap.legend.file') },
  { key: 'group', swatch: 'explore-legend-swatch explore-legend-swatch--group', color: null, label: t('explore.treemap.legend.group') },
  { key: 'depth', swatch: 'explore-legend-depth', color: null, label: t('explore.treemap.legend.depth') },
  ...(currentVolume.value?.capabilities.snapshots
    ? [{ key: 'snapshot', swatch: 'explore-legend-swatch explore-legend-swatch--hatch', color: null, label: t('explore.treemap.snapshot') }]
    : []),
  { key: 'unknown', swatch: 'explore-legend-swatch explore-legend-swatch--unknown', color: null, label: t('explore.treemap.unknown') },
])
/** The tile the previous, deeper level was, so the map zooms out of it. */
const mapFocusKey = ref<string | null>(null)
const headerMeasure = computed(() => {
  const host = gridHost.value
  if (!host) return undefined
  const style = getComputedStyle(host)
  return textMeasure(`${style.fontWeight} ${style.fontSize} ${style.fontFamily}`)
})
const reclaimableLabel = computed(() => t(cleanupUnavailable.value ? 'explore.table.reclaimableNoCleanup' : 'explore.table.reclaimable'))
const fixedColumns = computed(() => ({
  physical: headerWidth(80, t('explore.table.size'), headerMeasure.value),
  reclaimable: headerWidth(96, reclaimableLabel.value, headerMeasure.value),
  held: headerWidth(128, t('explore.table.held'), headerMeasure.value),
}))
/** The DataGrid's selection column. */
const SELECT_COLUMN = 40
// Kept free even before the rows overflow, so the columns do not jump when they do.
const scrollbar = scrollbarWidth()
const NAME_MIN_WIDTH = 150
const textColumns = computed(() => {
  const { physical, reclaimable, held } = fixedColumns.value
  const columns = listColumns(gridRoom(gridSize.value.width, scrollbar) - SELECT_COLUMN - physical - reclaimable, held)
  return { ...columns, name: Math.max(NAME_MIN_WIDTH, columns.name) }
})
/** Held by and Where to manage are folded into the name's second line. */
const compactList = computed(() => textColumns.value.held == null)

const columns = computed<DataGridColumn[]>(() => [
  {
    key: 'name', label: t('explore.table.name'), minWidth: compactList.value ? NAME_MIN_WIDTH : NAME_FULL_MIN,
    width: textColumns.value.name,
    sortable: true,
    compare: (left, right) => String(left).localeCompare(String(right), undefined, { numeric: true, sensitivity: 'base' }),
  },
  { key: 'physical', label: t('explore.table.size'), width: fixedColumns.value.physical, align: 'right', sortable: true, compare: compareBytes },
  { key: 'reclaimable', label: reclaimableLabel.value, width: fixedColumns.value.reclaimable, align: 'right', sortable: true, compare: compareBytes },
  ...(textColumns.value.held
    ? [{ key: 'held', label: t('explore.table.held'), width: textColumns.value.held, sortable: true }]
    : []),
  ...(textColumns.value.where
    ? [{ key: 'location', label: t('explore.table.where'), width: textColumns.value.where, sortable: true }]
    : []),
])

/** Rows the grid shows checked: in the cleanup list, or inside a folder that is. */
const checkedRowKeys = computed(() => basketItems.value.length
  ? gridRows.value.flatMap(row => isChecked(row.entry) ? [row.id] : [])
  : [])

/** The header box and row boxes change the cleanup list, never a separate selection. */
function changeSelectedKeys(keys: string[]) {
  const next = new Set(keys)
  const before = new Set(checkedRowKeys.value)
  const byKey = new Map(gridRows.value.map(row => [row.id, row.entry]))
  const removed = [...before].filter(key => !next.has(key))
  const added = [...next].filter(key => !before.has(key))
  for (const key of removed) {
    const entry = byKey.get(key)
    if (entry && isExactBasketItem(entry)) changeSelection(entry, false)
  }
  // Folders first: a folder folds what it contains, so its items are never counted twice.
  const additions = added.flatMap(key => byKey.get(key) ?? []).sort((left, right) => left.path.length - right.path.length)
  let folded = 0
  let folder: ExploreRowEntry | null = null
  for (const entry of additions) {
    const count = changeSelection(entry, true)
    if (count > 0) { folded += count; folder = entry }
  }
  if (folded && folder) {
    toast.info(t('explore.basket.folded', { folder: pathName(folder.path), count: formatCount(folded) }))
  }
}

watch(() => selectedVolume.value?.mount.path, () => {
  // A page that linked here (Packages) may ask for a path of the volume.
  requestedTarget.value = (currentVolume.value ? takeExploreRequest() : null) ?? currentVolume.value?.mount ?? null
  search.value = ''
  chosenReport.value = null
}, { immediate: true })

watch(() => [currentTarget.value?.path, currentTarget.value?.path_hex, view.value], () => {
  // A chosen report answers only the paths it covers; elsewhere the newest report does.
  const mount = currentVolume.value?.mount.path
  const target = currentTarget.value?.path
  if (chosenReport.value && mount && target && history.value
    && !reportsCovering(history.value, mount, target).some(scan => scan.job_id === chosenReport.value)) chosenReport.value = null
  loadTop()
}, { immediate: true })
watch(chosenReport, () => loadTop())
// A scan publish or a finished cleanup changes what the reports describe (P0-3).
watch(answersVersion, () => loadTop())

async function loadTop() {
  const target = currentTarget.value
  if (!target) return
  navigation?.abort()
  const controller = new AbortController()
  navigation = controller
  const serial = ++requestSerial.value
  loading.value = true
  requestError.value = null
  const params = {
    target: target.path_hex ? target : target.path,
    kind: view.value,
    by: 'reclaimable' as const,
    limit: TOP_LIMIT,
    ...(chosenReport.value ? { report: chosenReport.value } : {}),
  }
  try {
    const [answer, saved] = await Promise.all([api('top.list', params, { signal: controller.signal }), volumeHistory()])
    const shares = sharesWithReports(answer, saved, target)
    // Only the last navigation may touch the screen; the previous level stays
    // visible until this one has an answer (design review §7.3).
    if (serial !== requestSerial.value) return
    history.value = saved
    shareReports.value = shares
    metricsAttempted.value = new Set()
    shownTarget.value = target
    document.value = answer
    if (shares.length) void loadShareMetrics(controller.signal)
  } catch (error) {
    // A superseded navigation must never reach the screen, not even its error.
    if (serial !== requestSerial.value) return
    document.value = null
    shareReports.value = []
    shownTarget.value = null
    requestError.value = error instanceof ApiError ? error : new ApiError('unknown')
  } finally {
    if (serial === requestSerial.value) loading.value = false
  }
}

/** The volume's saved scans, read once per volume and answer version; null when history fails. */
function volumeHistory(): Promise<HistoryDocument | null> {
  const key = `${currentVolume.value?.mount.path ?? ''}\u0000${answersVersion.value}`
  if (historyRead?.key !== key) historyRead = { key, answer: api('history.list', {}).catch(() => null) }
  return historyRead.answer
}

/**
 * At a volume root, the shares a share scan left a report for: every one
 * without a volume report, else those scanned after it, unless the user chose
 * a report. Their root answers then say which report answers each share.
 */
function sharesWithReports(answer: TopDocument, saved: HistoryDocument | null, target: Path): ShareReport[] {
  const mount = currentVolume.value?.mount.path
  if (!mount || !saved || pathKey(target) !== pathKey(currentVolume.value!.mount)) return []
  if (answer.errors.some(error => error.code === 'no_report')) return scannedShares(saved, mount).map(path => ({ path, root: null }))
  const finished = answer.results[0]?.report.scan_finished
  if (chosenReport.value || !finished) return []
  return newerScannedShares(saved, mount, finished).map(path => ({ path, root: null }))
}

/**
 * The root totals of the next page of share reports (design review 2026-10-01
 * §1): names show first, numbers follow one page at a time, not one call per share.
 */
async function loadShareMetrics(signal?: AbortSignal) {
  const serial = requestSerial.value
  const paths = pendingShareMetrics.value
  if (!paths.length) return
  const attempted = new Set(metricsAttempted.value)
  for (const path of paths) attempted.add(path)
  metricsAttempted.value = attempted
  const answers = await Promise.all(paths.map(async path => {
    const entry = shareReports.value.find(item => item.path.path === path)
    return { path, root: entry ? await shareRoot(entry.path, signal) : null }
  }))
  if (serial !== requestSerial.value) return
  for (const answer of answers) {
    const entry = shareReports.value.find(item => item.path.path === answer.path)
    if (entry) entry.root = answer.root
  }
}

/** The share report's answer at the share root, for its totals; null when it cannot be read. */
async function shareRoot(path: Path, signal?: AbortSignal) {
  try {
    const answer = await api(
      'top.list',
      { target: path.path_hex ? path : path.path, kind: 'all', by: 'reclaimable', limit: 1 },
      signal ? { signal } : undefined,
    )
    return answer.results[0] ?? null
  } catch {
    return null
  }
}

/** A row for a share, or for a path a share scan reported on that volumes.list does not list as a share. */
function browseEntry(path: Path, root: TopResult | null = null, isShare = true): BrowseShareEntry {
  return {
    browseOnly: true,
    path: path.path,
    ...(path.path_hex ? { path_hex: path.path_hex } : {}),
    kind: 'dir',
    flags: isShare ? ['subvolume_root'] : [],
    metrics: root
      ? { physical: root.target.metrics.physical, reclaimable: root.target.metrics.reclaimable }
      : { physical: null, reclaimable: null },
    held_by: root ? heldByOfRoot(root.target) : null,
    service: null,
    cleanup_eligible: false,
  }
}

/** Why a share row cannot be selected: scanned on its own, or not scanned at all. */
function browseNote(entry: BrowseShareEntry) {
  return t(entry.metrics.physical == null ? 'explore.row.browseOnly' : 'explore.row.shareReport')
}

function pathName(path: string) {
  return path.replace(/\/$/, '').split('/').at(-1) || path
}

function rowContext(entry: ExploreRowEntry) {
  const folder = coveringFolder(entry)
  const goesWith = folder ? t('explore.row.goesWith', { folder: pathName(folder.path) }) : ''
  const scanned = 'browseOnly' in entry && entry.scanned ? t('report.row.shareScan', { time: formatRelative(entry.scanned) }) : ''
  const context = joinParts([scanned, goesWith, itemCount(entry), pathContext(entry)].filter(Boolean), locale)
  if (!('within' in entry) || !entry.within) return context
  const included = t('explore.row.includedIn', { folder: pathName(entry.within.path) })
  return joinParts([included, context].filter(Boolean), locale)
}

/** A folder's items from the scan: its subtree's nodes less the folder itself. */
function itemCount(entry: ExploreRowEntry) {
  if (entry.kind !== 'dir' || entry.service || !('entries' in entry.metrics)) return ''
  return t('explore.row.items', { count: formatCount(entry.metrics.entries - 1) })
}

function pathContext(entry: ExploreRowEntry) {
  if (!('browseOnly' in entry) && entry.service) {
    return t('explore.row.servicePath', { path: pathName(entry.path) })
  }
  const parent = entry.path.replace(/\/$/, '').split('/').slice(0, -1).join('/')
  const target = currentTarget.value?.path.replace(/\/$/, '')
  if (target && parent.startsWith(`${target}/`)) return parent.slice(target.length + 1)
  return flagLabel(entry)
}

function pathKey(path: { path: string; path_hex?: string }) {
  if (path.path_hex) return path.path_hex
  let hex = pathHex.get(path.path)
  if (hex == null) {
    if (pathHex.size >= 10_000) pathHex.clear()
    hex = Array.from(new TextEncoder().encode(path.path), byte => byte.toString(16).padStart(2, '0')).join('')
    pathHex.set(path.path, hex)
  }
  return hex
}

function entryIcon(entry: ExploreRowEntry) {
  return entry.kind === 'dir' ? Folder : entry.kind === 'service' ? Settings2 : File
}

function entryOf(row: Record<string, unknown>) {
  return row.entry as ExploreRowEntry
}

function changeView(value: string) {
  if (value === 'all' || value === 'dirs' || value === 'files') view.value = value
}

function changeVolume(path: string) {
  volumes.selected = path
}

function navigateBreadcrumb(item: BreadcrumbItem) {
  const index = Number(item.id)
  const crumb = crumbs.value[index]
  const left = crumbs.value[index + 1]
  mapFocusKey.value = left ? pathKey(left) : null
  if (crumb) requestedTarget.value = crumb
  search.value = ''
}

/** Why File Station cannot show an item, or '' when it can: only DSM's listed shared folders open. */
function fileStationReason(entry: ExploreRowEntry) {
  const block = fileStationBlock(entry)
  return block ? t(fileStationBlockKey(block)) : ''
}

/** A folder opens in File Station as itself, a file selected in its folder; otherwise a toast says why not. */
function openEntryInFileStation(entry: ExploreRowEntry) {
  const reason = fileStationReason(entry)
  if (reason) toast.info(reason)
  else if (!openInFileStation(entry)) toast.info(t('common.fileStation.failed'))
}

function openFolder(entry: ExploreRowEntry) {
  if (entry.kind !== 'dir' || entry.service) return false
  requestedTarget.value = { path: entry.path, ...(entry.path_hex ? { path_hex: entry.path_hex } : {}) }
  mapFocusKey.value = null
  search.value = ''
  return true
}

function heldBytes(entry: ExploreEntry, className: string) {
  return entry.held_by.reduce((sum, held) => sum + (held.class === className ? held.bytes : 0), 0)
}

function heldLabel(entry: ExploreRowEntry) {
  if (!entry.held_by) return t('common.unknown')
  const knownClasses = ['snapshot', 'recycle_bin', 'hard_link', 'reflink', 'pending_cleaner', 'open_file', 'unknown']
  const held = entry.held_by.filter(item => item.bytes > 0)
  if (!held.length) return t('explore.row.noHolds')
  return held.map(item => t(`explore.held.${knownClasses.includes(item.class) ? item.class : 'unknown'}`, { bytes: formatBytes(item.bytes) })).join(', ')
}

function locationLabel(entry: ExploreRowEntry) {
  if (!('browseOnly' in entry) && entry.service)
    return t('explore.row.managedAt', { service: serviceLabel(entry), place: placeLabel(entry) })
  return placeLabel(entry)
}

/** Where to manage without the managing service, which the name column already shows. */
function placeLabel(entry: ExploreRowEntry) {
  if ('browseOnly' in entry) return browseNote(entry)
  if (entry.service) return manageInLabel(entry.service.manage_in_id)
  const placeId = entry.where_to_fix.manage_in_id
  if (placeId) return manageInLabel(placeId)
  const refused = refusedKind(entry)
  if (refused) return t(`explore.where.protected.${refused}`)
  const token = entry.where_to_fix.token
  const knownTokens = ['cleanup', 'snapshot_replication', 'recycle_bin', 'shared', 'inspect_explain', 'wait', 'why_not_freed', 'doctor', 'package']
  return t(knownTokens.includes(token) ? `explore.where.${token}` : 'explore.where.unknown')
}

/** What protects a row cleanup refuses although its fix reads "cleanup": the cell names that, not the cleanup list. */
function refusedKind(entry: ExploreEntry) {
  if (entry.cleanup_eligible || entry.service || entry.where_to_fix.token !== 'cleanup' || cleanupUnavailable.value) return null
  return protectedKind(entry.path, currentVolume.value?.mount.path ?? '', entry.flags)
}

/** The place on the cell's first line, what to do there on the second. */
function placeLines(entry: ExploreRowEntry) {
  const [place, ...action] = placeLabel(entry).split(LABEL_SEPARATOR)
  return { place, action: action.join(LABEL_SEPARATOR) }
}

function serviceLabel(entry: ExploreEntry) {
  return serviceName(entry.service?.id)
}

function manageInLabel(id: string) {
  const ids = [
    'drive_admin_console', 'container_manager', 'surveillance_station', 'owning_package',
    'shared_folder_recycle_bin', 'active_backup_business', 'owning_folder', 'hyper_backup',
  ]
  return t(ids.includes(id) ? `explore.manageIn.${id}` : 'explore.manageIn.unknown')
}

function entryKindLabel(entry: ExploreRowEntry) {
  const key = entry.kind === 'dir' ? 'folder' : entry.kind === 'file' ? 'file' : entry.kind === 'service' ? 'service' : 'other'
  return t(`explore.kind.${key}`)
}

function flagLabel(entry: ExploreRowEntry) {
  if (!entry.flags.length) return entryKindLabel(entry)
  const knownFlags = ['system', 'recycle', 'encrypted', 'subvolume_root', 'own_state', 'unlinked_subvolume', 'snapshot_not_read']
  return joinParts(entry.flags.map(flag => t(`explore.flag.${knownFlags.includes(flag) ? flag : 'unknown'}`)), locale)
}

function isChecked(entry: ExploreRowEntry) {
  if ('browseOnly' in entry) return false
  return basket.value?.has(entry) ?? false
}

function isExactBasketItem(entry: ExploreRowEntry) {
  return basketKeySet.value.has(pathKey(entry))
}

function isSelectionDisabled(entry: ExploreRowEntry) {
  if ('browseOnly' in entry || cleanupUnavailable.value) return true
  const coveredByFolder = isChecked(entry) && !isExactBasketItem(entry)
  return entry.kind === 'service' || !entry.cleanup_eligible && !isExactBasketItem(entry) || coveredByFolder
}

/** The folder in the cleanup list this row goes with, when the row itself is not in it. */
function coveringFolder(entry: ExploreRowEntry) {
  if ('browseOnly' in entry || !isChecked(entry) || isExactBasketItem(entry)) return null
  const key = pathKey(entry)
  return basketItems.value.find(item => key.startsWith(`${pathKey(item)}2f`)) ?? null
}

/** Why a row cannot be selected, in words that say what to do instead. */
function selectionReason(entry: ExploreRowEntry) {
  if (!isSelectionDisabled(entry)) return undefined
  if ('browseOnly' in entry) return browseNote(entry)
  if (cleanupUnavailable.value && currentVolume.value) return cleanupUnavailableText(currentVolume.value)
  const folder = coveringFolder(entry)
  if (folder) return t('explore.row.reason.covered', { folder: pathName(folder.path) })
  if (entry.service) return t('explore.row.reason.service', { service: serviceLabel(entry) })
  const refused = refusedKind(entry)
  if (refused) return t(`explore.row.reason.protected.${refused}`)
  return t('explore.row.reason.elsewhere', { place: placeLabel(entry) })
}

function rowSelectable(row: Record<string, unknown>) {
  return !isSelectionDisabled(entryOf(row))
}

function rowSelectReason(row: Record<string, unknown>) {
  return selectionReason(entryOf(row))
}

// Row callbacks the grid and the map take are functions of the page, not
// arrows made in the template: a new arrow on every render made each move of
// the split draw all of the list's rows again.
function activateRow(row: ExploreCellRow) {
  onActivate((row as ExploreGridRow).entry)
}

function fileStationRow(row: ExploreCellRow) {
  openEntryInFileStation((row as ExploreGridRow).entry)
}

function rowSelectLabel(row: Record<string, unknown>) {
  return selectionLabel(entryOf(row))
}

function zoomableEntry(entry: ExploreRowEntry) {
  return entry.kind === 'dir' && !entry.service
}

function selectionLabel(entry: ExploreRowEntry) {
  if ('browseOnly' in entry) return browseNote(entry)
  const key = isExactBasketItem(entry) ? 'explore.basket.remove' : 'explore.basket.select'
  return t(key, { name: pathName(entry.path) })
}

/** Adds or removes a row; for an add, how many listed items the folder folded in (-1 when not added). */
function changeSelection(entry: ExploreRowEntry, selected: boolean) {
  if ('browseOnly' in entry) return -1
  if (!basket.value) return -1
  if (!selected) {
    basket.value.remove(entry)
    return -1
  }
  if (cleanupUnavailable.value || entry.kind === 'service' || !entry.cleanup_eligible || (entry.tree == null && entry.inode_device == null)) return -1
  const fsid = document.value?.results[0]?.report.fsid
  if (!fsid) return -1
  const before = basket.value.items.value.length
  const added = basket.value.add({
    path: entry.path,
    ...(entry.path_hex ? { path_hex: entry.path_hex } : {}),
    expect: entry.tree == null
      ? { filesystem: 'ext4', filesystem_uuid: fsid, tree: null, inode_device: entry.inode_device!, objectid: entry.objectid }
      : { filesystem_uuid: fsid, tree: entry.tree, objectid: entry.objectid },
    reclaimable: entry.metrics.reclaimable,
    snapshotHeldBytes: heldBytes(entry, 'snapshot'),
  })
  return added ? before + 1 - basket.value.items.value.length : -1
}

function onActivate(entry: ExploreRowEntry) {
  if (openFolder(entry)) return
  if ('browseOnly' in entry) return
  if (entry.kind === 'service' && entry.service) {
    openPackage(entry.service.id, entry.path)
    return
  }
  // A file with nothing more to say than its row opens in File Station; one
  // with notes about it opens a sheet built from the row, with no NAS call.
  if (!entryNotes(entry).length) {
    openEntryInFileStation(entry)
    return
  }
  detailEntry.value = entry
  detailOpen.value = true
}

function compareBytes(left: unknown, right: unknown) {
  if (left == null && right == null) return 0
  if (left == null) return -1
  if (right == null) return 1
  return Number(left) - Number(right)
}

const detailEntryName = computed(() => detailEntry.value ? pathName(detailEntry.value.path) : '')
const detailNotes = computed(() => detailEntry.value ? entryNotes(detailEntry.value).map(notice => code('notice', notice)) : [])
const detailSize = computed(() => {
  const metrics = detailEntry.value?.metrics
  if (!metrics) return ''
  const size = formatBytes(metrics.physical)
  return metrics.reclaimable == null
    ? t('explore.detail.sizeFreeUnknown', { size })
    : t('explore.detail.size', { size, reclaimable: formatBytes(metrics.reclaimable) })
})
/** Where to manage this path, kept in the detail sheet when the column is hidden. */
const detailManageText = computed(() => {
  const entry = detailEntry.value
  if (!entry || 'browseOnly' in entry) return ''
  if (entry.service) return t('explore.detail.managedBy', { service: serviceLabel(entry) })
  const placeId = entry.where_to_fix.manage_in_id
  return placeId ? t('explore.detail.manageIn', { place: manageInLabel(placeId) }) : locationLabel(entry)
})
</script>

<template>
  <div ref="pageHost" class="explore-page flex h-full min-h-0 min-w-0 flex-col overflow-hidden bg-[var(--ui-surface)]">
    <VolumeCapabilities v-if="currentVolume" :volume="currentVolume" />
    <ScanBadge v-if="chipReport" :report="chipReport" :scope="chipScope" :consistency="compactPartial ? 'partial' : undefined" :details="statusDetails" />

    <!-- The volume is chosen in the page bar, before the status; one volume offers no choice. -->
    <VolumePicker
      v-bind="{ ariaLabel: t('common.volumePicker.label') }" placement="bar" :model-value="currentVolume?.mount.path ?? ''"
      :volumes="volumeChoices" @update:model-value="changeVolume"
    />

    <div class="flex min-h-0 flex-1 flex-col gap-[var(--ui-section-gap)] overflow-hidden">
      <Breadcrumb v-if="crumbs.length > 1" class="shrink-0" :items="breadcrumbItems" :aria-label="t('explore.breadcrumb.label')" @click-item="navigateBreadcrumb" />
      <!-- One row: the view tabs' strip runs up to the search, so the list starts higher. -->
      <div class="explore-toolbar flex shrink-0 items-end gap-3">
        <SectionTabs class="min-w-0 flex-1" :model-value="view" :items="viewTabs" @update:model-value="changeView" />
        <div class="flex min-w-0 shrink-0 items-center gap-1 pb-0.5">
          <ReportPicker
            v-if="showReportPicker && currentVolume"
            v-model="chosenReport"
            data-testid="explore-report"
            :reports="coveringReports"
            :mount="currentVolume.mount.path"
            :mixed="newerShares.length > 0"
          />
          <SearchField v-model="search" variant="filter" :placeholder="t('explore.search.placeholder')" :clear-label="t('explore.search.clear')" />
          <PageInfo data-testid="page-info" inline side="left" :notes="pageNotes" />
        </div>
      </div>

      <div v-if="stateKind === 'no_report'" class="flex min-h-0 flex-1 items-center justify-center">
        <StateView kind="no_report" :last-scan-seconds="lastScanSeconds">
          <Button variant="primary" @click="openNewScan">{{ t('explore.state.scanNow') }}</Button>
        </StateView>
      </div>

      <template v-else>
        <StateView v-if="stateKind && !compactPartial" :kind="stateKind" :codes="stateCodes" :error="requestError">
          <Button v-if="canRetry" variant="ghost" @click="loadTop()">{{ t('common.retry') }}</Button>
        </StateView>
        <!-- The header freshness chip already carries a stale report's age; no separate info row. -->
        <Callout v-if="report?.consistency === 'changed-during-scan' && !report.attribution?.approximation" tone="info" :title="t('explore.state.changedTitle')">
          {{ t('explore.state.changedBody') }}
          <SystemAreasNote :report="report" :shares="currentVolume?.shares ?? []" />
        </Callout>
        <Callout v-if="sharesOnly" :dismissible="false" tone="info" :title="t('explore.state.sharesOnlyTitle')">
          {{ t('explore.state.sharesOnlyBody') }}
          <template #actions>
            <Button v-if="pendingShareMetrics.length" size="sm" variant="ghost" @click="loadShareMetrics()">
              {{ t('explore.state.loadMetrics', { count: pendingShareMetrics.length }) }}
            </Button>
          </template>
        </Callout>

        <div v-if="!stateOnly" ref="layoutHost" class="explore-layout flex min-h-0 flex-1 flex-col overflow-hidden" :class="{ 'explore-layout--stacked': stackedLayout }">
          <SplitPane
            class="min-h-0 flex-1"
            :storage-key="SPLIT_KEY"
            :model-value="splitShare"
            :orientation="splitOrientation"
            :default-size="splitDefault"
            :min-start="split.minStart"
            :min-end="split.minEnd"
            :gap="EXPLORE_SPLIT_GAP"
            :label="t('explore.split.label')"
            @update:model-value="changeSplit"
          >
            <template #start>
              <section ref="mapPanel" class="explore-map-panel flex min-h-0 flex-1 flex-col gap-2 rounded-[var(--ui-radius-lg)] border border-[var(--ui-border)] bg-[var(--ui-surface)] p-3">
                <div class="flex min-w-0 items-center justify-between gap-2 whitespace-nowrap">
                  <h2 class="min-w-0 truncate text-[length:var(--ui-font-size)] font-semibold text-[var(--ui-text)]">{{ t('explore.treemap.title') }}</h2>
                  <div class="flex shrink-0 items-center gap-2">
                    <span v-if="!legendCollapsed" class="text-[12px] text-[var(--ui-text-muted)]">{{ t('explore.treemap.area') }}</span>
                    <!-- A low or narrow map keeps its key behind an icon, so the tiles keep their room. -->
                    <InfoTip v-if="legendCollapsed" data-testid="explore-legend-tip" :aria-label="t('explore.treemap.legendLabel')">
                      <p class="explore-legend-area">{{ t('explore.treemap.area') }}</p>
                      <ul class="explore-legend" :aria-label="t('explore.treemap.legendLabel')">
                        <li v-for="item in legendItems" :key="item.key"><span :class="item.swatch" :style="item.color ? { '--hue': item.color } : undefined" />{{ item.label }}</li>
                      </ul>
                    </InfoTip>
                    <DropdownMenu v-if="!cleanupUnavailable" :items="worthItems" align="end" @select="openWorth">
                      <template #trigger>
                        <Button size="sm" variant="ghost" class="explore-worth" :aria-label="t('explore.worth.title')">
                          <Lightbulb class="size-3.5" aria-hidden="true" />{{ t('explore.worth.title') }}<span v-if="worthALook.length" class="explore-worth-count">{{ formatCount(worthALook.length) }}</span>
                        </Button>
                      </template>
                    </DropdownMenu>
                  </div>
                </div>
                <LoadingMask class="explore-map flex min-h-0 min-w-0 flex-1" :loading="loading" :text="t('common.loading')">
                  <Treemap
                    v-if="hasTreemapTiles"
                    :entries="filteredRows"
                    :selected-keys="basketKeys"
                    :zoomable="zoomableEntry"
                    :focus-key="mapFocusKey"
                    @activate="onActivate"
                  />
                  <div v-else-if="!loading" class="explore-map-empty" data-testid="explore-treemap-empty"><EmptyState :title="t('explore.treemap.empty')" /></div>
                </LoadingMask>
                <ul v-if="!legendCollapsed" class="explore-legend" :aria-label="t('explore.treemap.legendLabel')">
                  <li v-for="item in legendItems" :key="item.key"><span :class="item.swatch" :style="item.color ? { '--hue': item.color } : undefined" />{{ item.label }}</li>
                </ul>
              </section>
            </template>

            <template #end>
              <section class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-[var(--ui-radius-lg)] border border-[var(--ui-border)] bg-[var(--ui-surface)]">
                <div class="flex shrink-0 items-center justify-between gap-3 border-b border-[var(--ui-border)] bg-[var(--ui-surface-subtle)] px-3 py-2">
                  <h2 class="text-[length:var(--ui-font-size)] font-semibold text-[var(--ui-text)]">{{ t('explore.table.title') }}</h2>
                </div>
                <div ref="gridHost" class="explore-data-grid min-h-0 flex-1 overflow-hidden">
                  <ReclaimGrid
                    v-model:sort="sort"
                    table-id="explore"
                    class="h-full"
                    :rows="gridRows"
                    :columns="columns"
                    row-key="id"
                    selectable
                    select-all-scope="all"
                    :selected-keys="checkedRowKeys"
                    :row-selectable="rowSelectable"
                    :row-select-disabled-reason="rowSelectReason"
                    :select-row-label="rowSelectLabel"
                    :page-size="50"
                    :loading="loading"
                    :sticky-header="true"
                    :empty-label="search ? t('explore.table.noMatches') : t('explore.table.empty')"
                    @update:selected-keys="changeSelectedKeys"
                  >
                    <!-- Slots written out, not a v-for over their names: worked-out names would draw the list again with every draw of the page. -->
                    <template #cell-name="{ row }"><ExploreCell column="name" :row="row as ExploreGridRow" :compact="compactList" @activate="activateRow" @file-station="fileStationRow" /></template>
                    <template #cell-physical="{ row }"><ExploreCell column="physical" :row="row as ExploreGridRow" :largest="largestPhysical" /></template>
                    <template #cell-reclaimable="{ row }"><ExploreCell column="reclaimable" :row="row as ExploreGridRow" /></template>
                    <template #cell-held="{ row }"><ExploreCell column="held" :row="row as ExploreGridRow" :compact="compactList" /></template>
                    <template #cell-location="{ row }"><ExploreCell column="location" :row="row as ExploreGridRow" :compact="compactList" /></template>
                  </ReclaimGrid>
                </div>
              </section>
            </template>
          </SplitPane>
        </div>
      </template>

      <BasketBar
        v-if="basketCount > 0"
        :count-label="t('explore.basket.count', { count: basketCount })"
        :summary-lead="compactBasket
          ? t(basketExact ? 'explore.basket.shortLeadExact' : 'explore.basket.shortLead', { count: basketCount })
          : t(basketExact ? 'explore.basket.summaryLeadExact' : 'explore.basket.summaryLead', { count: basketCount })"
        :estimate-text="formatBytes(basketBytes)"
        :summary-after="compactBasket ? undefined : t('explore.basket.summaryAfter')"
        :summary-details="compactBasket ? undefined : basketSummaryDetails"
        :summary-title="basketSummaryTitle"
        :compact="compactBasket"
        :clear-label="t('explore.basket.clear')"
        :review-label="t('explore.basket.review')"
        emphasis
        @clear="basket?.clear()"
        @review="reviewOpen = true"
      >
        <template v-if="volumeSpace" #meter>
          <StorageMeter
            :class="compactBasket ? 'explore-basket-meter explore-basket-meter--compact' : 'explore-basket-meter'"
            :total="volumeSpace.total"
            :free="volumeSpace.free"
            :freeing="basketBytes"
            :at-least="!basketExact"
            :pending="basketEstimate.pending.value"
            :labels="{ freeNow: t('explore.basket.freeNow'), freeAfter: t('explore.basket.freeAfter') }"
            :format="formatBytes"
          />
        </template>
      </BasketBar>
    </div>

    <Sheet v-model:open="detailOpen" :title="t('explore.detail.title', { name: detailEntryName })" :description="detailEntry?.path" v-bind="detailEntry?.path ? {} : { 'aria-describedby': undefined }" :close-label="t('common.close')">
      <p class="explore-detail-size" data-testid="explore-detail-size">{{ detailSize }}</p>
      <ul class="explore-detail-notes" data-testid="explore-detail-notes">
        <li v-for="note in detailNotes" :key="note">{{ note }}</li>
      </ul>
      <p v-if="detailManageText" class="explore-detail-manage" data-testid="explore-detail-manage">{{ detailManageText }}</p>
      <div v-if="detailEntry" class="explore-detail-actions">
        <Button v-if="!fileStationReason(detailEntry)" size="sm" variant="secondary" data-testid="explore-detail-file-station" @click="openEntryInFileStation(detailEntry)">
          <FolderOpen class="size-4" aria-hidden="true" />{{ t('common.fileStation.open') }}
        </Button>
        <p v-else class="explore-detail-reason" data-testid="explore-detail-file-station-reason">{{ fileStationReason(detailEntry) }}</p>
      </div>
    </Sheet>

    <ReviewDialog v-model:open="reviewOpen" />
  </div>
</template>

<style scoped>
/* The DSM build's CSS has no .m-auto, so a flex wrapper centres the note. */
.explore-map-empty {
  display: flex;
  flex: 1 1 auto;
  min-width: 0;
  min-height: 0;
  align-items: center;
  justify-content: center;
}

/* Narrow content: the treemap sits above the table; both keep a minimum and
   the separator between them still moves. */
.explore-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 14px;
  margin: 0;
  padding: 0;
  list-style: none;
  color: var(--ui-text-secondary);
  font-size: 12px;
  line-height: 16px;
}
.explore-legend li {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.explore-legend-area {
  margin: 0 0 6px;
  font-size: 12px;
  color: var(--ui-text-secondary);
}
.explore-legend-swatch {
  width: 14px;
  height: 12px;
  border-radius: var(--ui-radius-sm);
  background: color-mix(in oklab, var(--hue) 30%, var(--ui-surface));
}
/* Kind keys are drawn in neutral grey: shape, not hue, tells a folder from a file. */
.explore-legend-swatch--folder {
  background: color-mix(in oklab, var(--ui-gray-11) 16%, var(--ui-surface));
  box-shadow: inset 0 4px 0 color-mix(in oklab, var(--ui-gray-11) 42%, var(--ui-surface));
}
.explore-legend-swatch--file {
  background: color-mix(in oklab, var(--ui-gray-11) 16%, var(--ui-surface));
}
.explore-legend-swatch--group {
  background: var(--ui-surface-subtle);
  border: 1px dashed var(--ui-border-strong);
}
.explore-legend-swatch--hatch {
  background: repeating-linear-gradient(135deg, color-mix(in srgb, var(--ui-text) 22%, var(--ui-surface)) 0 2px, var(--ui-surface-subtle) 2px 5px);
  box-shadow: inset 0 0 0 1px var(--ui-border);
}
.explore-legend-swatch--unknown {
  background: repeating-linear-gradient(-45deg, color-mix(in srgb, var(--ui-text) 12%, var(--ui-surface)) 0 1.5px, var(--ui-surface) 1.5px 5px);
  box-shadow: inset 0 0 0 1px var(--ui-border);
}
.explore-legend-depth {
  width: 28px;
  height: 12px;
  border-radius: var(--ui-radius-sm);
  background: linear-gradient(90deg, color-mix(in oklab, var(--ui-blue-9) 12%, var(--ui-surface)), color-mix(in oklab, var(--ui-blue-9) 56%, var(--ui-surface)));
}
.explore-worth-count {
  min-width: 16px;
  margin-left: 2px;
  padding: 0 4px;
  border-radius: var(--ui-radius-pill);
  background: color-mix(in srgb, var(--ui-primary) 14%, transparent);
  color: var(--ui-primary);
  font-size: 11px;
  line-height: 16px;
  text-align: center;
}
/* Every row is a name and one quieter line, so rows keep one height and the
   list scrolls to row boundaries, never stopping with a row cut in half. */
.explore-data-grid {
  --ui-grid-row-height: 44px;
}
.explore-data-grid :deep([data-ui-scroll-edges-viewport]) {
  scroll-snap-type: y proximity;
  scroll-padding-top: var(--ui-grid-header-height);
}
.explore-data-grid :deep(tr[data-row-key]) {
  scroll-snap-align: start;
}
/* One line of text over the bar: the projection reads at a glance. */
.explore-basket-meter {
  width: max-content;
  min-width: 240px;
  max-width: 100%;
}
/* Compact, the meter says only where the cleanup leaves the volume (its
   accessible name keeps both), so the bar stays one row. */
.explore-basket-meter--compact {
  min-width: 0;
}
.explore-basket-meter--compact :deep([data-ui-storage-meter-now]),
.explore-basket-meter--compact :deep([data-ui-storage-meter-now] + span) {
  display: none;
}
.explore-detail-actions {
  margin-top: 12px;
}
.explore-detail-manage {
  margin: 12px 0 0;
  color: var(--ui-text-secondary);
  font-size: var(--ui-font-size-sm);
}
.explore-detail-size {
  margin: 0 0 12px;
  color: var(--ui-text);
  font-size: var(--ui-font-size);
}
.explore-detail-notes {
  margin: 0;
  padding-left: 20px;
  list-style: disc;
  color: var(--ui-text);
  font-size: var(--ui-font-size);
}
.explore-detail-notes > li + li {
  margin-top: 8px;
}
.explore-detail-reason {
  margin: 0;
  color: var(--ui-text-secondary);
  font-size: var(--ui-font-size-sm);
}
</style>
