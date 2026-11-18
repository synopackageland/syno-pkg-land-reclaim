<script setup lang="ts">
// Duplicates: identical folders and big duplicate files, one row per group.
// A volume lists its shares; a share lists its groups biggest-first in two
// lists, folders and large files, the large files under one filter. A row
// names the copies kept; clicking it opens every copy of the group on the
// volume, ticked for those that move to the Recycle Bin. A group is always
// seen on its whole volume: a share's page only picks which groups it lists,
// and cleaning a group keeps one copy and moves every other, whichever share
// it is in. Identical folders that keep the same folder are one row. A row's
// own box puts its groups in the volume's cleanup list, and Review sends them
// to cleanup.plan. A group that would move a copy out of a share without a
// Recycle Bin cannot be selected; the share's Control Panel button is next to
// it. No preview: a copy opens in File Station.
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { Badge, Breadcrumb, Button, Callout, CellText, EmptyState, SegmentControl, SegmentItem, SizeBar, StorageMeter, LoadingMask, useToast } from '@synopackageland/ui'
import type { BreadcrumbItem, DataGridColumn } from '@synopackageland/ui'
import { PageInfo } from '@synopackageland/sdk/vue'
import { Copy, FolderOpen, ScanSearch, Star } from '@lucide/vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import BasketBar from '@/components/BasketBar.vue'
import VolumePicker from '@/components/VolumePicker.vue'
import { COMPACT_BASKET_BELOW } from '@/components/listLayout'
import { fileStationBlock, fileStationBlockKey, openInFileStation } from '@/components/fileStation'
import { useElementSize } from '@/composables/useElementSize'
import { ReviewDialog } from '@/pages/registry'
import { useBasket } from '@/stores/basket'
import { openNewScan } from '@/stores/nav'
import { jobs } from '@/stores/jobs'
import { scanJobOnVolume } from '@/components/scanProgress'
import { selectedVolume, volumeList, volumes } from '@/stores/volumes'
import { formatBytes, formatCount, formatVolume, joinParts, locale, t } from '@/i18n'
import { recycleBins, type RecycleBins } from '@/dsm/recycleBins'
import type { DuplicateGroup, DuplicatePage, DuplicateScopeDocument, DuplicateTally } from '@/types/duplicates'
import { DuplicatesUnavailable, PAGE_LIMIT, analysisUnavailable, analyze, duplicatesScope, keepCopy, needsScan } from './duplicatesClient'
import { checkGroup, copyKind, DUPLICATE_FILTERS, duplicateRow, filterCategory, groupSelection, keptCount, mergeRows, offShares, rowDetail, rowName, scopeName, sharePath, unmovableText, type DuplicateFilter, type DuplicateRow, type KeepChange } from './duplicatesModel'
import KeepSheet from './KeepSheet.vue'
import SelectAllDialog, { type SelectAllState } from './SelectAllDialog.vue'
import ShareSettingsButton from '@/components/ShareSettingsButton.vue'

const toast = useToast()
const POLL_MS = 1500

const currentVolume = computed(() => selectedVolume.value)
const mount = computed(() => currentVolume.value?.mount.path ?? '')
const basket = computed(() => mount.value ? useBasket(mount.value) : null)
const share = ref<string | null>(null)
/** The share's list shown: identical folders, or large files under `filter`. */
type ListTab = 'folders' | 'files'
const tab = ref<ListTab>('folders')
/** The user picked a tab in this share: the first answer no longer picks it. */
let tabChosen = false
const filter = ref<DuplicateFilter>('all')
/** The scope folder the API gets; the volume when left out. */
const scopePath = computed(() => share.value === null ? undefined : `${mount.value.replace(/\/+$/, '')}/${share.value}`)

const volumeAnswer = shallowRef<DuplicateScopeDocument | null>(null)
/** The share's identical folders: they have no category, so no filter changes them. */
const foldersPage = shallowRef<DuplicatePage<DuplicateGroup> | null>(null)
const filesAnswer = shallowRef<DuplicateScopeDocument | null>(null)
const folderGroups = shallowRef<DuplicateGroup[]>([])
const fileGroups = shallowRef<DuplicateGroup[]>([])
const loading = ref(false)
const failed = ref(false)
/** Why the answer failed, as the backend said it. */
const failedDetail = ref<string | null>(null)
const unavailable = ref(false)
let request = 0
/** The analysis this page started, until its outcome is known. */
let startedJob: string | null = null
let pollTimer: ReturnType<typeof setTimeout> | undefined

const answer = computed(() => share.value === null ? volumeAnswer.value : filesAnswer.value)
const analysis = computed(() => answer.value?.analysis ?? null)
const analysisRunning = computed(() => !!analysis.value?.running)
/** The numbers shown are the previous ones: preferred shares changed, or an analysis runs. */
const recalculating = computed(() => !!answer.value?.recalculating || analysisRunning.value)
const scope = computed(() => scopeName(mount.value, share.value))

/** Each share's Recycle Bin, read fresh from DSM; null when DSM did not say. */
const bins = shallowRef<RecycleBins | null>(null)
let binsRequest = 0
async function loadBins() {
  const id = ++binsRequest
  const next = await recycleBins()
  if (id === binsRequest) bins.value = next
}
/** A share without a Recycle Bin: cleanup deletes its copies for good, once Review confirms that. */
const binOff = (name: string) => bins.value?.get(name)?.enabled === false
/** Whether a group can go in the cleanup list, and why not: the one rule rows and Select all follow. */
const check = (group: DuplicateGroup) => checkGroup(group)
const groupSelectable = (group: DuplicateGroup) => check(group).block === null

/**
 * The new rows of a list where the shown rows stay in place: a row the user
 * just changed is still where they changed it, so they can see and undo it.
 * Rows the answer adds come after; a row it no longer has on this page stays
 * as shown (it may lie on a later page). Leaving or reloading the page sorts.
 */
function inPlace(shown: DuplicateGroup[], next: DuplicateGroup[]): DuplicateGroup[] {
  const byId = new Map(next.map(group => [group.group.id, group]))
  const ids = new Set(shown.map(group => group.group.id))
  return [...shown.map(group => byId.get(group.group.id) ?? group), ...next.filter(group => !ids.has(group.group.id))]
}

/**
 * Loads the scope. A share comes in one request: its large files under the
 * filter, and with All also its identical folders (the volume's answer and
 * each share's under All are cached on the NAS after an analysis). Another
 * filter reloads only the large files. `stable` keeps the shown rows in place.
 */
async function load(quiet = false, filesOnly = false, stable = false) {
  clearTimeout(pollTimer)
  const volume = mount.value
  if (!volume) return
  const id = ++request
  const path = scopePath.value
  const category = filterCategory(filter.value)
  if (!quiet) loading.value = true
  failed.value = false
  failedDetail.value = null
  try {
    if (path === undefined) {
      // The volume's tallies and its shares; its groups are listed per share.
      const whole = await duplicatesScope({ volume, limit: 1 })
      if (id !== request) return
      volumeAnswer.value = whole
    } else {
      const both = !category && !filesOnly
      const answer = await duplicatesScope({ volume, path, list: both ? 'both' : 'files', category, limit: PAGE_LIMIT })
      if (id !== request) return
      filesAnswer.value = answer
      fileGroups.value = stable ? inPlace(fileGroups.value, answer.groups.items) : answer.groups.items
      if (both) {
        foldersPage.value = answer.folders ?? null
        folderGroups.value = stable ? inPlace(folderGroups.value, answer.folders?.items ?? []) : answer.folders?.items ?? []
        // A share opens on its folders when it has any.
        if (!tabChosen) tab.value = folderGroups.value.length ? 'folders' : 'files'
      }
    }
    unavailable.value = false
    if (recalculating.value) pollTimer = setTimeout(() => void load(true), POLL_MS)
    else if (startedJob) {
      // The analysis this page started has ended: one that left no results says why.
      const job = startedJob
      startedJob = null
      if (analysis.value?.state === 'none' && await analysisUnavailable(job).catch(() => false) && id === request) unavailable.value = true
    }
  } catch (error) {
    if (id !== request) return
    if (error instanceof DuplicatesUnavailable && needsScan(error.detail)) unavailable.value = true
    else {
      failed.value = true
      failedDetail.value = error instanceof DuplicatesUnavailable ? error.detail ?? null : null
    }
  } finally {
    if (id === request) loading.value = false
  }
}

/** The next page of a list, after the groups already shown. */
const loadingMore = ref(false)
async function loadMore(list: 'folders' | 'files') {
  const shown = list === 'folders' ? folderGroups : fileGroups
  const path = scopePath.value
  if (!path) return
  loadingMore.value = true
  try {
    const next = await duplicatesScope({ volume: mount.value, path, list, category: list === 'files' ? filterCategory(filter.value) : undefined, offset: shown.value.length, limit: PAGE_LIMIT })
    if (scopePath.value === path) shown.value = [...shown.value, ...next.groups.items]
  } catch {
    toast.danger(t('duplicates.state.failed'))
  } finally {
    loadingMore.value = false
  }
}

watch([mount, share], () => {
  foldersPage.value = null
  filesAnswer.value = null
  folderGroups.value = []
  fileGroups.value = []
  tabChosen = false
  if (share.value === null) volumeAnswer.value = null
  void load()
  void loadBins()
}, { immediate: true })
// The filter is the large files': it reloads them only. All comes back
// with the folders in the share's one cached answer.
watch(filter, value => {
  fileGroups.value = []
  void load(false, value !== 'all')
})
watch(mount, () => {
  share.value = null
  filter.value = 'all'
})
// The filter belongs to the large files: leaving their list clears it.
watch(tab, value => {
  if (value === 'folders' && filter.value !== 'all') filter.value = 'all'
})
function changeTab(value: string | undefined) {
  if (value !== 'folders' && value !== 'files') return
  tabChosen = true
  tab.value = value
}
function changeFilter(value: string | undefined) {
  if (value && (DUPLICATE_FILTERS as string[]).includes(value)) filter.value = value as DuplicateFilter
}
onBeforeUnmount(() => clearTimeout(pollTimer))

// The analysis: none yet, older than the newest scan, or running.
const starting = ref(false)
async function startAnalysis() {
  starting.value = true
  try {
    startedJob = (await analyze(mount.value)).job_id ?? null
    await load(true)
  } catch {
    toast.danger(t('duplicates.analysis.failed'))
  } finally {
    starting.value = false
  }
}
const notAnalyzed = computed(() => analysis.value?.state === 'none' && !analysisRunning.value)
const stale = computed(() => analysis.value?.state === 'stale' && !analysisRunning.value)
/** Preferred shares changed and no analysis runs: start it again here. */
const needsAnalysis = computed(() => !!answer.value?.recalculating && !analysisRunning.value && !notAnalyzed.value)

// The page bar: the volume, then the page's notes. They list only exclusions Reclaim really detects.
const volumeChoices = computed(() => volumeList.value.map(volume => ({ value: volume.mount.path, label: formatVolume(volume.mount.path) })))
const pageNotes = computed(() => [t('duplicates.info.what'), t('duplicates.info.junk'), t('duplicates.info.partial'), t('duplicates.info.keep'), t('duplicates.info.excluded')])

const breadcrumbItems = computed<BreadcrumbItem[]>(() => [
  { id: 'volume', display: formatVolume(mount.value) },
  ...(share.value ? [{ id: 'share', display: share.value }] : []),
])
function navigateBreadcrumb(item: BreadcrumbItem) {
  if (item.id === 'volume') share.value = null
}
function openShare(name: string) {
  share.value = name
  filter.value = 'all'
}

// Volume: the shares with duplicates. Each row is what cleaning only that share would do.
type ShareRow = { id: string; share: string; tally: DuplicateTally; can_free: number }
const shareRows = computed<ShareRow[]>(() => (volumeAnswer.value?.children ?? []).map(child => ({ id: child.path.path, share: child.name.path, tally: child.tally, can_free: child.tally.all.can_free })))
const largestShare = computed(() => Math.max(0, ...shareRows.value.map(row => row.can_free)))
const shareColumns = computed<DataGridColumn[]>(() => [
  { key: 'share', label: t('duplicates.shares.name'), flex: 1, minWidth: 160 },
  ...(bins.value ? [{ key: 'bin', label: t('duplicates.shares.recycleBin'), width: 240, tooltip: (row: Record<string, unknown>) => binOff(String(row.share)) ? t('duplicates.shares.binOffDeletes') : undefined }] : []),
  { key: 'folders', label: t('duplicates.shares.folders'), width: 190 },
  { key: 'files', label: t('duplicates.shares.files'), width: 200 },
  { key: 'can_free', label: t('duplicates.table.canFree'), width: 170, align: 'right' },
])
const copiesOf = (count: number) => count ? t('duplicates.shares.copies', { count }) : '—'

// Share: its groups under the filter, biggest first.
const folderRows = computed<DuplicateRow[]>(() => mergeRows(folderGroups.value.map(group => duplicateRow(group, mount.value, scope.value)), mount.value))
const fileRows = computed<DuplicateRow[]>(() => fileGroups.value.map(group => duplicateRow(group, mount.value, scope.value)))
const moreFolders = computed(() => Math.max(0, (foldersPage.value?.total ?? 0) - folderGroups.value.length))
const moreFiles = computed(() => Math.max(0, (filesAnswer.value?.groups.total ?? 0) - fileGroups.value.length))
const groupColumns = computed<DataGridColumn[]>(() => [
  { key: 'name', label: t('duplicates.table.name'), flex: 1.3, minWidth: 200 },
  { key: 'kept', label: t('duplicates.table.kept'), flex: 1.2, minWidth: 200 },
  { key: 'moves', label: t('duplicates.table.moves'), width: 190, tooltip: row => joinParts([movesText(row as unknown as DuplicateRow), movesDetail(row as unknown as DuplicateRow)].filter(Boolean), locale) },
  { key: 'canFree', label: t('duplicates.table.canFree'), width: 150, align: 'right', tooltip: row => joinParts([formatBytes(Number(row.canFree)), String(row.sizeText)].filter(Boolean), locale) },
])
function keptCell(row: DuplicateRow) {
  const count = row.members.length > 1 ? 1 : keptCount(row.group)
  return count > 1 ? { text: t('duplicates.row.keptMany', { count }), secondary: joinParts([row.keptText, row.keptWhy], locale) } : { text: row.keptText, secondary: row.keptWhy }
}
/** Under the name: every copy, where they are; a partly identical folder says what it shares. */
const nameDetail = (row: DuplicateRow) => rowDetail(row)
/** Shares without a Recycle Bin the row removes copies from: those copies are deleted for good. */
function rowOffShares(row: DuplicateRow): string[] {
  return [...new Set(row.members.flatMap(group => offShares(group, bins.value)))]
}
function movesText(row: DuplicateRow) {
  return row.removes ? t('duplicates.row.moves', { count: row.removes }) : t('duplicates.row.noMove')
}
/** Under the count: what a partly identical folder shares, and where the copies go: the Recycle Bin, or deleted for good without one. */
function movesDetail(row: DuplicateRow) {
  const where = !row.removes ? '' : rowOffShares(row).length ? t('duplicates.row.deletes') : t('duplicates.row.toBin')
  return joinParts([row.group.partial ? row.copiesText : '', where].filter(Boolean), locale)
}

// The cleanup list holds groups of the volume: a row is on when all its selectable groups are in it.
const inBasket = computed(() => new Set(basket.value?.duplicates.value.groups.map(group => group.id) ?? []))
const rowIn = (row: DuplicateRow) => {
  const wanted = row.members.filter(groupSelectable)
  return wanted.length > 0 && wanted.every(group => inBasket.value.has(group.group.id))
}
const selectedKeys = (rows: DuplicateRow[]) => rows.filter(rowIn).map(row => row.id)
const folderKeys = computed(() => selectedKeys(folderRows.value))
const fileKeys = computed(() => selectedKeys(fileRows.value))
const listCounts = computed(() => ({ folders: formatCount(foldersPage.value?.total ?? 0), files: formatCount(filesAnswer.value?.groups.total ?? 0) }))
/** Only a row with a copy to remove goes in the cleanup list: not one whose other copies all stay, nor one that would move a copy out of a share without a Recycle Bin. */
const rowSelectable = (row: Record<string, unknown>) => !recalculating.value && (row as unknown as DuplicateRow).members.some(groupSelectable)

function addGroups(all: DuplicateGroup[]) {
  const groups = all.filter(groupSelectable)
  if (!basket.value || !groups.length) return
  basket.value.addDuplicates(groups.map(group => groupSelection(group, mount.value)))
}
function changeSelected(rows: DuplicateRow[], keys: string[]) {
  const wanted = new Set(keys)
  addGroups(rows.filter(row => wanted.has(row.id) && !rowIn(row)).flatMap(row => row.members))
  const dropped = rows.filter(row => !wanted.has(row.id) && rowIn(row)).flatMap(row => row.members.map(group => group.group.id))
  if (dropped.length) basket.value?.removeDuplicates(dropped)
}
const rowLabel = (row: Record<string, unknown>) => t('duplicates.table.selectRow', { name: String(row.name) })

// Select all follows the scope and the filter, and says so.
const selectAllLabel = computed(() => share.value === null
  ? t('duplicates.selectAll.volume', { scope: scope.value })
  : t(`duplicates.selectAll.${filter.value}`, { scope: scope.value }))
const visibleCount = computed(() => (foldersPage.value?.total ?? 0) + (filesAnswer.value?.groups.total ?? 0))
const selectableRows = computed(() => [...(filter.value === 'all' ? folderRows.value : []), ...fileRows.value].filter(row => row.members.some(groupSelectable)))
const allSelected = computed(() => share.value !== null && selectableRows.value.length > 0
  && selectableRows.value.every(rowIn)
  && (filter.value !== 'all' || moreFolders.value === 0) && moreFiles.value === 0)

// Select all: every group of the scope under the filter, read page by page
// and checked one by one in a modal that counts them and can be stopped.
// Only groups that can be cleaned up are selected; the others are listed
// with why, and so are copies that stay. It never ends on a refusal: what
// could be selected is.
const selectAllOpen = ref(false)
const selectAll = ref<SelectAllState>({ phase: 'done', done: 0, total: null, selected: 0, skipped: [], stays: [] })
let selectAllRun = 0
const SKIPS_SHOWN = 200
watch(selectAllOpen, isOpen => {
  if (!isOpen) selectAllRun++
})
function stopSelectAll() {
  selectAllRun++
  if (selectAll.value.phase === 'checking') selectAll.value = { ...selectAll.value, phase: 'stopped' }
}
/** The lists Select all reads: on a share, the folders (under All) and the large files under the filter; on the volume, both. */
function selectAllLists(): { list: 'folders' | 'files'; category?: ReturnType<typeof filterCategory> }[] {
  const category = filterCategory(filter.value)
  if (share.value === null) return [{ list: 'folders' }, { list: 'files' }]
  return [...(category ? [] : [{ list: 'folders' as const }]), { list: 'files' as const, category }]
}
async function startSelectAll() {
  if (recalculating.value || selectAllOpen.value) return
  const run = ++selectAllRun
  const volume = mount.value
  const path = scopePath.value
  const state: SelectAllState = { phase: 'checking', done: 0, total: null, selected: 0, skipped: [], stays: [] }
  selectAll.value = state
  selectAllOpen.value = true
  const live = () => run === selectAllRun && mount.value === volume && scopePath.value === path
  const seen = new Set<string>()
  try {
    for (const { list, category } of selectAllLists()) {
      for (let offset = 0; ; offset += PAGE_LIMIT) {
        const page = await duplicatesScope({ volume, path, list, category, offset, limit: PAGE_LIMIT })
        if (!live()) return
        state.total = (state.total ?? 0) + (offset === 0 ? page.groups.total : 0)
        const add: DuplicateGroup[] = []
        for (const group of page.groups.items) {
          state.done++
          if (seen.has(group.group.id)) continue
          seen.add(group.group.id)
          const result = check(group)
          for (const stay of result.unmovable) {
            if (state.stays.length < SKIPS_SHOWN) state.stays.push({ id: `${group.group.id}:${stay.path}`, path: sharePath(stay.path, volume), why: unmovableText(stay, volume, bins.value) })
          }
          if (!result.block) {
            add.push(group)
            continue
          }
          if (state.skipped.length >= SKIPS_SHOWN) continue
          state.skipped.push({ id: group.group.id, name: rowName(group), why: t('duplicates.why.nothing') })
        }
        addGroups(add)
        state.selected += add.length
        selectAll.value = { ...state }
        if (!page.groups.items.length || offset + PAGE_LIMIT >= page.groups.total) break
      }
    }
    if (!live()) return
    state.phase = 'done'
    selectAll.value = { ...state }
    // Nothing to tell: the selection speaks for itself.
    if (!state.skipped.length && !state.stays.length) selectAllOpen.value = false
  } catch {
    if (!live()) return
    selectAll.value = { ...state, phase: 'stopped' }
    toast.danger(t('duplicates.state.failed'))
  }
}

// The sheet's Apply: store which copies stay, then put the group in the cleanup list with its new row.
function replaceGroup(next: DuplicateGroup | null, id: string) {
  const swap = (list: DuplicateGroup[]) => next ? list.map(group => group.group.id === id ? next : group) : list.filter(group => group.group.id !== id)
  folderGroups.value = swap(folderGroups.value)
  fileGroups.value = swap(fileGroups.value)
}
/** A keep is being stored: no other change until the NAS answers. */
const pendingKeep = ref(false)
async function applyKeep(group: DuplicateGroup, changes: KeepChange[]) {
  if (pendingKeep.value) return
  const id = group.group.id
  pendingKeep.value = true
  let next: DuplicateGroup | null = group
  try {
    for (const change of changes) next = (await keepCopy({ volume: mount.value, group_id: id, path: change.path, keep: change.keep, scope: scopePath.value })).group
  } catch {
    toast.danger(t('duplicates.state.keepFailed'))
    return
  } finally {
    pendingKeep.value = false
  }
  if (changes.length) replaceGroup(next, id)
  // The list follows the new row: one with nothing left to move leaves it.
  basket.value?.removeDuplicates([id])
  if (next) addGroups([next])
  // New totals, rows left where they are: a row that now frees nothing is not sorted away.
  if (changes.length) void load(true, false, true)
}
function applyMembers(add: DuplicateGroup[], drop: DuplicateGroup[]) {
  if (drop.length) basket.value?.removeDuplicates(drop.map(group => group.group.id))
  addGroups(add)
}

function openCopyInFileStation(path: string, group: DuplicateGroup) {
  const target = { path, kind: copyKind(group.group) }
  const block = fileStationBlock(target)
  if (block) toast.info(t(fileStationBlockKey(block)))
  else if (!openInFileStation(target)) toast.info(t('common.fileStation.failed'))
}

// A row's copies (or a merged row's folders), in a Sheet opened from it.
const detailRow = shallowRef<DuplicateRow | null>(null)
const detailOpen = ref(false)
function openDetail(row: Record<string, unknown>) {
  detailRow.value = row as unknown as DuplicateRow
  detailOpen.value = true
}
// The row shown follows its group's new answer.
watch([folderRows, fileRows], ([folders, files]) => {
  const shown = detailRow.value
  if (!shown) return
  detailRow.value = [...folders, ...files].find(row => row.id === shown.id || row.members.some(group => group.group.id === shown.group.group.id)) ?? shown
})
function dropGroups(groups: DuplicateGroup[]) {
  basket.value?.removeDuplicates(groups.map(group => group.group.id))
}

// The scope line: what Select all takes, in words.
const totals = computed(() => answer.value?.totals?.all ?? null)
const scopeTitle = computed(() => t('duplicates.scope.title', { scope: scope.value, size: formatBytes(totals.value?.can_free ?? 0) }))
const scopeDetail = computed(() => {
  const current = answer.value
  if (!current?.totals) return ''
  const unchecked = (current.unchecked?.files ?? 0) + (current.unchecked?.folders ?? 0)
  return joinParts([
    t('duplicates.scope.copies', { count: current.totals.all.copies }),
    ...(share.value === null
      ? [t('duplicates.scope.overlap')]
      : [t('duplicates.scope.groups', { count: visibleCount.value })]),
    ...(current.totals.all.frees_later ? [t('duplicates.scope.later', { size: formatBytes(current.totals.all.frees_later) })] : []),
    ...(unchecked ? [t('duplicates.scope.unchecked', { count: unchecked })] : []),
  ], locale)
})
const analyzed = computed(() => !!analysis.value && analysis.value.state !== 'none')
const empty = computed(() => !loading.value && !failed.value && !unavailable.value && analyzed.value && (share.value === null
  ? shareRows.value.length === 0
  : filesAnswer.value != null && !folderRows.value.length && !fileRows.value.length))
const showLists = computed(() => !unavailable.value && !notAnalyzed.value && !empty.value)
/** The first analysis of the volume: nothing to list yet. */
const firstRun = computed(() => analysisRunning.value && analysis.value?.state === 'none')
/** A scan of the whole volume runs: the page bar shows it, and the page has nothing to add until there are results. */
const volumeScan = computed(() => {
  const job = mount.value ? scanJobOnVolume(jobs.list, mount.value) : undefined
  return job && job.targets.some(target => target.path === mount.value) ? job : undefined
})
const scanHides = computed(() => !!volumeScan.value && !showLists.value)
const showScope = computed(() => showLists.value && !firstRun.value && !scanHides.value)

// The cleanup bar, as on Explore, counting copies and groups.
const pageHost = ref<HTMLElement | null>(null)
const pageSize = useElementSize(pageHost, { width: Infinity, height: Infinity })
const compactBasket = computed(() => pageSize.value.height < COMPACT_BASKET_BELOW.height || pageSize.value.width < COMPACT_BASKET_BELOW.width)
const basketGroups = computed(() => basket.value?.duplicates.value.groups ?? [])
const otherItems = computed(() => basket.value?.items.value.length ?? 0)
const basketCopies = computed(() => otherItems.value + (basket.value?.duplicateCopies.value ?? 0))
const basketBytes = computed(() => basket.value?.lowerBound.value ?? 0)
const basketLead = computed(() => otherItems.value
  ? t('explore.basket.summaryLead', { count: basket.value?.count.value ?? 0 })
  : t('duplicates.basket.lead', { count: basketCopies.value, groups: t('duplicates.basket.groups', { count: basketGroups.value.length }) }))
const basketDetails = computed(() => t('duplicates.basket.keepsOne'))
const volumeSpace = computed(() => {
  const volume = currentVolume.value
  return volume?.total_bytes != null && volume.free_bytes != null ? { total: volume.total_bytes, free: volume.free_bytes } : null
})
const reviewOpen = ref(false)
function changeVolume(path: string) {
  volumes.selected = path
}
</script>

<template>
  <div ref="pageHost" class="duplicates-page flex h-full min-h-0 min-w-0 flex-col gap-[var(--ui-section-gap)] overflow-hidden bg-[var(--ui-surface)]">
    <VolumePicker
      v-bind="{ ariaLabel: t('common.volumePicker.label') }" placement="bar" :model-value="mount"
      :volumes="volumeChoices" @update:model-value="changeVolume"
    />
    <PageInfo data-testid="page-info" side="bottom" align="start" :notes="pageNotes" />

    <Breadcrumb v-if="share !== null" class="shrink-0" data-testid="duplicates-breadcrumb" :items="breadcrumbItems" :aria-label="t('duplicates.breadcrumb.label')" @click-item="navigateBreadcrumb" />

    <div v-if="showScope" class="duplicates-scope flex shrink-0 flex-wrap items-center gap-3" :class="{ 'duplicates-stale': recalculating }">
      <div class="min-w-0 flex-1">
        <div class="text-[length:var(--ui-font-size)] font-semibold text-[var(--ui-text)]" data-testid="duplicates-scope-title">{{ scopeTitle }}</div>
        <div class="text-[12px] text-[var(--ui-text-secondary)]" data-testid="duplicates-scope-detail">{{ scopeDetail }}</div>
      </div>
      <Button
        :variant="allSelected ? 'secondary' : 'primary'"
        :disabled="recalculating || loading || !showLists || allSelected"
        :loading="selectAllOpen && selectAll.phase === 'checking'"
        data-testid="duplicates-select-all"
        @click="startSelectAll"
      >
        {{ selectAllLabel }}
      </Button>
    </div>

    <Callout v-if="analysisRunning && !firstRun" :dismissible="false" tone="info" :title="t('duplicates.analysis.running')" data-testid="duplicates-running" />
    <Callout v-else-if="answer?.recalculating" :dismissible="false" tone="info" :title="t('duplicates.state.recalculating')" data-testid="duplicates-recalculating">
      <template v-if="needsAnalysis" #actions><Button size="sm" variant="ghost" :loading="starting" @click="startAnalysis">{{ t('duplicates.analysis.again') }}</Button></template>
    </Callout>
    <Callout v-else-if="stale" :dismissible="false" tone="warning" :title="t('duplicates.analysis.stale')" data-testid="duplicates-stale">
      <template #actions><Button size="sm" variant="ghost" :loading="starting" @click="startAnalysis">{{ t('duplicates.analysis.again') }}</Button></template>
    </Callout>
    <Callout v-if="failed" :dismissible="false" tone="danger" :title="t('duplicates.state.failed')">
      <p v-if="failedDetail" data-testid="duplicates-failed-detail">{{ failedDetail }}</p>
      <template #actions><Button size="sm" variant="ghost" @click="load()">{{ t('common.retry') }}</Button></template>
    </Callout>

    <div v-if="scanHides" class="duplicates-blank" data-testid="duplicates-scanning" />

    <div v-else-if="unavailable" class="duplicates-blank" data-testid="duplicates-unavailable">
      <EmptyState :title="t('duplicates.unavailable.title')" :description="t('duplicates.unavailable.body')" :icon="ScanSearch">
        <template #actions><Button variant="primary" @click="openNewScan">{{ t('duplicates.unavailable.scan') }}</Button></template>
      </EmptyState>
    </div>

    <div v-else-if="firstRun" class="duplicates-blank" data-testid="duplicates-first-run">
      <EmptyState :title="t('duplicates.analysis.running')" :icon="Copy" />
    </div>

    <div v-else-if="notAnalyzed" class="duplicates-blank" data-testid="duplicates-none">
      <EmptyState :title="t('duplicates.analysis.noneTitle')" :description="t('duplicates.analysis.noneBody')" :icon="Copy">
        <template #actions><Button variant="primary" :loading="starting" data-testid="duplicates-find" @click="startAnalysis">{{ t('duplicates.analysis.find') }}</Button></template>
      </EmptyState>
    </div>

    <div v-else-if="empty" class="duplicates-blank" data-testid="duplicates-empty">
      <EmptyState :title="t('duplicates.state.empty', { scope })" :icon="Copy" />
    </div>

    <section v-else-if="share === null" class="duplicates-panel flex min-h-0 flex-1 flex-col overflow-hidden">
      <div class="duplicates-panel__head"><h2>{{ t('duplicates.shares.title') }}</h2></div>
      <LoadingMask class="min-h-0 flex-1 overflow-hidden" :loading="loading" :text="t('common.loading')">
        <ReclaimGrid table-id="duplicates-shares" class="h-full" :rows="shareRows" :columns="shareColumns" row-key="id" :sticky-header="true">
          <template #cell-share="{ row }">
            <button type="button" class="duplicates-link" :data-testid="`duplicates-share-${row.share}`" @click="openShare(String(row.share))">
              <CellText :icon="FolderOpen" :text="String(row.share)"><span class="text-[var(--ui-primary)]">{{ row.share }}</span></CellText>
            </button>
          </template>
          <template #cell-bin="{ row }">
            <div v-if="binOff(String(row.share))" class="duplicates-bin-off">
              <Badge tone="warning" :data-testid="`duplicates-bin-${row.share}`">{{ t('duplicates.shares.binOffDeletes') }}</Badge>
              <ShareSettingsButton icon :share="String(row.share)" />
            </div>
            <Badge v-else-if="bins?.has(String(row.share))" tone="success" :data-testid="`duplicates-bin-${row.share}`">{{ t('duplicates.shares.binOn') }}</Badge>
          </template>
          <template #cell-folders="{ row }"><CellText :text="copiesOf((row as unknown as ShareRow).tally.folders.copies)" /></template>
          <template #cell-files="{ row }"><CellText :text="copiesOf((row as unknown as ShareRow).tally.files.copies)" /></template>
          <template #cell-can_free="{ row }">
            <div class="duplicates-size" :class="{ 'duplicates-stale': recalculating }">
              <CellText class="tabular-nums" :text="formatBytes(Number(row.can_free))"><span class="font-semibold text-[var(--ui-primary)]">{{ formatBytes(Number(row.can_free)) }}</span></CellText>
              <SizeBar :value="Number(row.can_free)" :max="largestShare" />
            </div>
          </template>
        </ReclaimGrid>
      </LoadingMask>
    </section>

    <section v-else class="duplicates-panel flex min-h-0 flex-1 flex-col overflow-hidden" data-testid="duplicates-lists">
      <div class="duplicates-panel__head">
        <SegmentControl :model-value="tab" :aria-label="t('duplicates.list.label')" size="sm" data-testid="duplicates-tabs" @update:model-value="changeTab">
          <SegmentItem value="folders" :count="listCounts.folders">{{ t('duplicates.folders.title') }}</SegmentItem>
          <SegmentItem value="files" :count="listCounts.files">{{ t('duplicates.files.title') }}</SegmentItem>
        </SegmentControl>
        <SegmentControl v-if="tab === 'files'" :model-value="filter" :aria-label="t('duplicates.filter.label')" size="sm" data-testid="duplicates-filter" @update:model-value="changeFilter">
          <SegmentItem v-for="value in DUPLICATE_FILTERS" :key="value" :value="value">{{ t(`duplicates.filter.${value}`) }}</SegmentItem>
        </SegmentControl>
        <span v-if="tab === 'files'">{{ t('duplicates.files.threshold') }}</span>
      </div>
      <div class="min-h-0 flex-1 overflow-hidden">
        <ReclaimGrid
          :key="tab"
          :table-id="`duplicates-${tab}`" class="duplicates-grid h-full" :rows="tab === 'folders' ? folderRows : fileRows" :columns="groupColumns" row-key="id"
          selectable :selected-keys="tab === 'folders' ? folderKeys : fileKeys" :select-row-label="rowLabel" :sticky-header="true" :loading="loading"
          :row-selectable="rowSelectable"
          :empty-label="t(tab === 'folders' ? 'duplicates.folders.empty' : 'duplicates.files.empty')"
          :data-testid="`duplicates-grid-${tab}`"
          @update:selected-keys="changeSelected(tab === 'folders' ? folderRows : fileRows, $event)"
          @row-click="openDetail"
        >
          <template #cell-name="{ row }"><CellText :icon="(row as DuplicateRow).icon" :text="String(row.name)" :secondary="nameDetail(row as DuplicateRow)" /></template>
          <template #cell-kept="{ row }"><CellText :icon="Star" :text="keptCell(row as DuplicateRow).text" :secondary="keptCell(row as DuplicateRow).secondary" /></template>
          <template #cell-moves="{ row }">
            <div class="duplicates-moves">
              <CellText class="duplicates-moves__text" :text="movesText(row as DuplicateRow)" :secondary="movesDetail(row as DuplicateRow)" :data-testid="`duplicates-moves-${row.id}`" />
              <ShareSettingsButton v-for="name in rowOffShares(row as DuplicateRow)" :key="name" icon :share="name" />
            </div>
          </template>
          <template #cell-canFree="{ row }">
            <CellText class="tabular-nums" :class="{ 'duplicates-stale': recalculating }" data-testid="duplicates-can-free" :text="formatBytes(Number(row.canFree))" :secondary="String(row.sizeText)"><span class="font-semibold text-[var(--ui-primary)]">{{ formatBytes(Number(row.canFree)) }}</span></CellText>
          </template>
        </ReclaimGrid>
      </div>
      <div v-if="tab === 'folders' ? moreFolders : moreFiles" class="duplicates-more">
        <Button size="sm" variant="ghost" :loading="loadingMore" :data-testid="`duplicates-more-${tab}`" @click="loadMore(tab)">{{ t('duplicates.more', { count: Math.min(tab === 'folders' ? moreFolders : moreFiles, PAGE_LIMIT) }) }}</Button>
      </div>
    </section>

    <BasketBar
      v-if="(basket?.count.value ?? 0) > 0"
      :count-label="t('duplicates.basket.count', { count: basketCopies })"
      :summary-lead="basketLead"
      :estimate-text="formatBytes(basketBytes)"
      :summary-after="compactBasket ? undefined : t('explore.basket.summaryAfter')"
      :summary-details="compactBasket ? undefined : t('explore.basket.summaryDetails', { details: basketDetails })"
      :summary-title="basketDetails"
      :compact="compactBasket"
      :clear-label="t('explore.basket.clear')"
      :review-label="recalculating ? undefined : t('explore.basket.review')"
      emphasis
      @clear="basket?.clear()"
      @review="reviewOpen = true"
    >
      <template v-if="volumeSpace" #meter>
        <StorageMeter
          class="duplicates-meter"
          :total="volumeSpace.total"
          :free="volumeSpace.free"
          :freeing="basketBytes"
          :pending="recalculating"
          :labels="{ freeNow: t('explore.basket.freeNow'), freeAfter: t('explore.basket.freeAfter') }"
          :format="formatBytes"
        />
      </template>
    </BasketBar>

    <KeepSheet
      v-model:open="detailOpen"
      :row="detailRow" :volume="mount" :scope-path="scopePath" :busy="recalculating || pendingKeep"
      :bins="bins" :selected="inBasket"
      @apply="applyKeep"
      @apply-members="applyMembers"
      @drop="dropGroups"
      @file-station="openCopyInFileStation"
    />
    <SelectAllDialog v-model:open="selectAllOpen" :state="selectAll" @stop="stopSelectAll" />
    <ReviewDialog v-model:open="reviewOpen" />
  </div>
</template>

<style scoped>
/* A group row is two lines on each side: keep them apart. */
.duplicates-grid {
  --ui-grid-row-height: 56px;
}
/* A row opens its copies. */
.duplicates-grid :deep(tbody tr) {
  cursor: pointer;
}
.duplicates-panel {
  border: 1px solid var(--ui-border);
  border-radius: var(--ui-radius-lg);
  background: var(--ui-surface);
}
.duplicates-panel__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
  padding: 8px 12px;
  border-bottom: 1px solid var(--ui-border);
  background: var(--ui-surface-subtle);
}
.duplicates-panel__head h2 {
  margin: 0;
  color: var(--ui-text);
  font-size: var(--ui-font-size);
  font-weight: 600;
}
.duplicates-panel__head span {
  color: var(--ui-text-secondary);
  font-size: 12px;
}
.duplicates-size {
  display: grid;
  gap: 4px;
  justify-items: end;
  width: 100%;
}
.duplicates-link {
  display: flex;
  min-width: 0;
  max-width: 100%;
  border-radius: var(--ui-radius-sm);
  text-align: left;
  outline: none;
}
.duplicates-link:focus-visible {
  box-shadow: 0 0 0 2px var(--ui-focus-ring);
}
/* While numbers are recalculated they stay, greyed: they are the old ones. */
.duplicates-stale,
.duplicates-stale :deep(*) {
  color: var(--ui-text-muted) !important;
}
.duplicates-more {
  display: flex;
  justify-content: center;
  flex-shrink: 0;
  padding: 4px;
  border-top: 1px solid var(--ui-border);
}
/* A state with nothing to list fills the page and sits in its middle. */
.duplicates-blank {
  display: flex;
  flex: 1 1 auto;
  min-height: 0;
  align-items: center;
  justify-content: center;
}
/* A row keeps its height: the text is cut (its tooltip has it whole) and the settings are icons beside it. */
.duplicates-moves,
.duplicates-bin-off {
  display: flex;
  flex-wrap: nowrap;
  align-items: center;
  gap: 4px;
  min-width: 0;
  width: 100%;
}
.duplicates-moves__text {
  flex: 1 1 auto;
  min-width: 0;
}
.duplicates-bin-off :deep(.ui-badge),
.duplicates-bin-off > :first-child {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.duplicates-meter {
  width: max-content;
  min-width: 240px;
  max-width: 100%;
}
</style>
