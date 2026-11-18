<script setup lang="ts">
// Review moves the cleanup list into the shared folders' Recycle Bins
// (SPEC 5.7): the plan groups the items by shared folder, and the user
// restores or empties a Recycle Bin in DSM. Items with no Recycle Bin (in no
// shared folder, or in one whose Recycle Bin is off) are deleted for good,
// only after a box saying so is ticked; a shared folder whose Recycle Bin is
// off says where to turn it on, and Recheck then moves its items instead.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { Badge, Button, Callout, CellStatus, InfoTip, KeyValueRow, LoadingArea } from '@synopackageland/ui'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import StateView from '@/components/states/StateView.vue'
import { api, ApiError } from '@/api'
import { runInBackground, type PlanningProgress } from '@/api/planning'
import { formatBytes, formatCount, formatDateTime, joinParts, locale, t } from '@/i18n'
import { openNewScan } from '@/stores/nav'
import { invalidateAnswers } from '@/stores/answers'
import { pollNow } from '@/stores/jobs'
import { authorizeRootWork } from '@/stores/authorization'
import { session } from '@/stores/session'
import { useBasket } from '@/stores/basket'
import type { BasketItem } from '@/stores/basket'
import { findSubmittedJob } from '@/stores/jobRecovery'
import { selectedVolume } from '@/stores/volumes'
import { useBasketEstimate } from '@/composables/useBasketEstimate'
import ShareSettingsButton from '@/components/ShareSettingsButton.vue'
import type { DuplicatePlanSummary } from '@/types/duplicates'
import type { TargetError } from '@/types/schema/cleanup-plan'
import { DUPLICATE_REFUSALS, cleanupApplyParams, conflictResolution, duplicateReview, planParams, type ConflictGroup, type DuplicateReviewLine } from './reviewLogic'

type Completion = 'complete' | 'partial' | 'failed' | 'busy' | 'paused' | 'cancelled' | string
type CleanupPath = { path: string; path_hex?: string }
/** `reason` (and `package`) say why a `protected_target` is protected. */
/** `group`: the selected duplicate group whose copy the path is; `conflict`: for keeper_conflict, the groups that disagree on the copy at the path. */
type CleanupError = CleanupPath & { code: string; reason?: TargetError['reason']; package?: string; group?: string; conflict?: TargetError['conflict'] }
type Size = { entries: number; files: number; allocated_bytes: number }
type PlanItem = Size & { path: CleanupPath; kind: 'file' | 'directory'; drive: boolean }
/** `items` is a preview of `item_count` items; `drive_items` of them are in Synology Drive folders. */
/**
 * `delete`: no Recycle Bin takes the items, so apply removes them for good;
 * `delete_reason` says why: a folder right below the volume that is no shared
 * folder, or a shared folder whose Recycle Bin is off.
 */
type PlanShare = Size & { share: CleanupPath; path: CleanupPath; recycle_bin: { enabled: boolean; admin_only: boolean }; delete?: boolean; delete_reason?: 'no_shared_folder' | 'recycle_bin_off'; item_count?: number; drive_items?: number; symlinks?: number; items: PlanItem[] }
type PlanResponse = {
  completion?: Completion
  errors?: CleanupError[]
  plan_id?: string | null
  /** How many items the plan moves; `paths` lists the first of them. */
  items?: number
  /** How many of them are removed for good: they have no Recycle Bin (in no shared folder, or its Recycle Bin is off). */
  deletes?: number
  paths?: CleanupPath[]
  size?: Size
  shares?: PlanShare[]
  /** What a duplicates selection added (docs/duplicates.md). */
  duplicates?: DuplicatePlanSummary
}
type AppliedTarget = { path: CleanupPath; share: CleanupPath; outcome: string; moved_bytes: number; remaining_entries: number; remaining: CleanupPath[]; detail: string | null }
type CleanupResult = {
  command?: string
  completion?: Completion
  errors?: { code: string; path?: string }[]
  moved?: number
  deleted?: number
  already_absent?: number
  partial?: number
  changed?: number
  failed?: number
  moved_bytes?: number
  deleted_bytes?: number
  /** Items in the plan; `targets` lists only the first of those that did not move. */
  items?: number
  targets?: AppliedTarget[]
  recycle_bins?: { share: CleanupPath; path: CleanupPath; admin_only: boolean; items: number; moved_bytes: number }[]
}
type CleanupProgress = {
  plan_id: string
  items_total: number
  items_done: number
  moved: number
  deleted?: number
  already_absent: number
  partial: number
  changed: number
  failed: number
  bytes_total: number
  bytes_moved: number
  calls_done?: number
  elapsed_s: number
  updated: string
}
type CleanupJob = { job_id: string; kind: 'cleanup-apply'; phase: 'execute'; state: string; reason?: string | null; cleanup?: CleanupProgress }

const props = defineProps<{ volume?: string; paths?: BasketItem[]; jobId?: string }>()
const open = defineModel<boolean>('open', { required: true })
const emit = defineEmits<{ close: [] }>()

/** `stopped`: the user stopped the plan's check; nothing failed. */
type Phase = 'idle' | 'empty' | 'disabled' | 'planning' | 'stopped' | 'ready' | 'no_report' | 'busy' | 'refused' | 'error' | 'applying' | 'result' | 'apply_error'
const phase = ref<Phase>('idle')
const plan = ref<PlanResponse | null>(null)
const planErrorMessage = ref('')
const applyErrorMessage = ref('')
const applyResult = ref<CleanupResult | null>(null)
const activeJobId = ref<string | null>(null)
const activeJobState = ref('running')
const jobProgress = ref<CleanupProgress | null>(null)
const jobReason = ref<string | null>(null)
const pollError = ref(false)
/** What the plan job checked so far in its current step, while it runs. */
const planProgress = ref<PlanningProgress | null>(null)
/** The plan job, once accepted: Stop cancels it. */
const planJobId = ref<string | null>(null)
const stoppingPlan = ref(false)
const cancelPending = ref(false)
const cancelError = ref(false)
const planRequest = ref(0)
let planningController = new AbortController()
let pollTimer: ReturnType<typeof setTimeout> | undefined
let disposed = false
const applySent = ref(false)
let pollFailures = 0
let submittedPlanId: string | null = null
let knownJobIds: string[] = []

const volumePath = computed(() => props.volume ?? selectedVolume.value?.mount.path ?? null)
const basket = computed(() => volumePath.value ? useBasket(volumePath.value) : null)
const selectedItems = computed(() => props.paths ?? basket.value?.items.value ?? [])
/** Groups the Duplicates page added, each seen on its whole volume; a Review of given paths has none. */
const duplicateSelection = computed(() => props.paths ? null : basket.value?.duplicates.value ?? null)
const duplicateSummary = computed(() => duplicateReview(
  duplicateSelection.value?.groups ?? [],
  new Set(deleteShares.value.map(share => share.share.path)),
))
const DUPLICATE_LINES = {
  move: ['review.duplicates.line', 'review.duplicates.linePreferred', 'review.duplicates.partialLine'],
  delete: ['review.duplicates.lineDelete', 'review.duplicates.lineDeletePreferred', 'review.duplicates.partialLineDelete'],
  mixed: ['review.duplicates.lineMixed', 'review.duplicates.lineMixedPreferred', 'review.duplicates.partialLineMixed'],
} as const
/** A group's line, saying whether its copies move to the Recycle Bin or are deleted for good. */
function duplicateLine(line: DuplicateReviewLine) {
  const keeps = line.keeps
  const [plain, preferred, partial] = DUPLICATE_LINES[line.removal]
  if (line.items != null) return t(partial, { count: line.removes, items: formatCount(line.items), keeps })
  return t(line.preferred ? preferred : plain, { count: line.removes, keeps })
}
const selectedSignature = computed(() => [
  ...selectedItems.value.map(item => JSON.stringify([item.path, item.path_hex, item.expect])),
  ...(duplicateSelection.value?.groups.map(group => `group:${group.id}`) ?? []),
].join('\u0001'))
/** A duplicates refusal of apply: it names the volume, not a selected item. */
const applyRefusal = ref<string | null>(null)
const refusalCodes = computed(() => [...(plan.value?.errors?.map(error => error.code) ?? []), ...(applyRefusal.value ? [applyRefusal.value] : [])])
const itemErrors = computed(() => (plan.value?.errors ?? []).filter(error => !DUPLICATE_REFUSALS.has(error.code)))
/** Refusals only a new scan (or a new look at the selection after one) can fix. */
const RESCAN_REFUSALS = ['target_changed', 'keeper_changed']
/** Duplicates refusals the selection itself causes: taking the groups out lets the rest go ahead. */
const CLEAR_DUPLICATES_REFUSALS = ['keeper_removed', 'keeper_conflict', 'duplicates_nothing', 'duplicates_unknown_group', 'duplicates_stale']
const canClearDuplicates = computed(() => phase.value === 'refused' && !!duplicateSelection.value?.groups.length
  && !canResolveConflicts.value && refusalCodes.value.some(code => CLEAR_DUPLICATES_REFUSALS.includes(code)))

/** The keeper conflicts the plan named, each with both of its groups. */
const keeperConflicts = computed(() => (plan.value?.errors ?? []).filter(error => error.code === 'keeper_conflict' && error.conflict))
const listedGroups = computed(() => new Set(duplicateSelection.value?.groups.map(group => group.id) ?? []))
/** Which groups to take out of the list to settle every keeper conflict, and which the user picks. */
const resolution = computed(() => conflictResolution(keeperConflicts.value, listedGroups.value))
const canResolveConflicts = computed(() => phase.value === 'refused' && (resolution.value.drop.length > 0 || resolution.value.choices.length > 0))
const pairKey = (pair: readonly ConflictGroup[]) => pair.map(group => group.id).sort().join(':')
/** The group the user picked to take out, for each pair that removes each other's kept copy. */
const picked = ref<Record<string, string>>({})
const allPicked = computed(() => resolution.value.choices.every(pair => !!picked.value[pairKey(pair)]))
/** Every group the fix takes out: the ones that would remove a kept copy, and the user's picks. */
const groupsToDrop = computed(() => [
  ...resolution.value.drop,
  ...resolution.value.choices.flatMap(pair => pair.filter(group => group.id === picked.value[pairKey(pair)])),
])
const resolveLabel = computed(() => {
  const groups = groupsToDrop.value
  const total = resolution.value.drop.length + resolution.value.choices.length
  if (total === 1 && groups.length === 1) return t('review.duplicates.dropConflict', { group: groups[0].name })
  if (groups.length && allPicked.value) return t('review.duplicates.dropConflicts', { count: groups.length - 1, group: groups[0].name, more: formatCount(groups.length - 1) })
  return t('review.duplicates.dropPicked', { count: total, groups: formatCount(total) })
})
/** What the fix took out of the list, said once the plan is ready; cleared when the dialog closes. */
const droppedGroups = ref<ConflictGroup[]>([])
/** The fix is settling conflicts: a plan that names more takes those groups out too, then checks again. */
const resolving = ref(false)
let resolveRounds = 0
/** A fix never plans more often than this: each round takes at least one group out. */
const MAX_RESOLVE_ROUNDS = 20

function dropConflictGroups(groups: ConflictGroup[]) {
  droppedGroups.value = [...droppedGroups.value, ...groups.filter(group => !droppedGroups.value.some(dropped => dropped.id === group.id))]
  picked.value = {}
  // The list changes, so the dialog plans again at once (the watch below).
  basket.value?.removeDuplicates(groups.map(group => group.id))
}

/** Takes the groups that would remove a kept copy out of the list, then checks again and stays open. */
function resolveConflicts() {
  const groups = groupsToDrop.value
  if (!groups.length || !allPicked.value) return
  resolving.value = true
  resolveRounds = 0
  dropConflictGroups(groups)
}

/** The user's pick in a pair of groups that each remove the other's kept copy. */
function pickConflictGroup(pair: readonly ConflictGroup[], group: ConflictGroup) {
  picked.value = { ...picked.value, [pairKey(pair)]: group.id }
}

/**
 * A duplicates cleanup moves copies as the scan saw them, and the copies
 * it moves are not checked again: what changed in them since moves too.
 */
const duplicatesScanWarning = computed(() => {
  // The scan the duplicate analysis used, not the plan's own check of the paths.
  const finished = plan.value?.duplicates?.scan_finished
  if (!duplicateSummary.value.lines.length || !finished) return ''
  const removes = duplicateSummary.value.lines.some(line => line.removal !== 'move')
  return t(removes ? 'review.duplicates.scanAgeDelete' : 'review.duplicates.scanAge', { time: formatDateTime(new Date(finished * 1000).toISOString()) })
})
const shares = computed(() => plan.value?.shares ?? [])
const movingShares = computed(() => shares.value.filter(share => !share.delete))
/** Symlinks among what moves, as the plan counted them: DSM's Recycle Bin does not keep them, as with File Station's delete. */
const binSymlinks = computed(() => movingShares.value.reduce((sum, share) => sum + (share.symlinks ?? 0), 0))
/** Shared folders whose Recycle Bin is off: turning it on and checking again moves their items instead. */
const binOffShares = computed(() => shares.value.filter(share => share.delete_reason === 'recycle_bin_off'))
/** Folders with no Recycle Bin: their items are removed for good. */
const deleteShares = computed(() => shares.value.filter(share => share.delete))
const deleteCount = computed(() => plan.value?.deletes ?? deleteShares.value.reduce((sum, share) => sum + shareCount(share), 0))
const planId = computed(() => plan.value?.plan_id ?? null)
const planPaths = computed(() => plan.value?.paths ?? [])
/** Items the plan moves: the count, as its lists are only a preview. */
const planCount = computed(() => plan.value?.items ?? planPaths.value.length)
const shareCount = (share: PlanShare) => share.item_count ?? share.items.length
const moveSize = computed(() => plan.value?.size ?? null)
const touchesDrive = computed(() => movingShares.value.some(share => (share.drive_items ?? 0) > 0 || share.items.some(item => item.drive)))
/** Lists in the review show this many lines, then how many more. */
const SHOWN = 100
const shownErrors = computed(() => itemErrors.value.slice(0, SHOWN))
const shownDuplicateLines = computed(() => duplicateSummary.value.lines.slice(0, SHOWN))
// What emptying the Recycle Bin then frees: the cleanup list's exact
// estimate from the scan report when it answers, else its lower bound.
const basketEstimate = useBasketEstimate(() => selectedItems.value)
const freeLater = computed(() => {
  return basketEstimate.exact.value ?? (basket.value?.lowerBound.value || null)
})
/** What the cleanup does: everything moves, everything goes for good, or some of each; null before a plan says. */
const removal = computed<'move' | 'delete' | 'mixed' | null>(() => {
  if (!plan.value) return null
  if (!deleteCount.value) return 'move'
  return moveCount.value ? 'mixed' : 'delete'
})
const canApply = computed(() => !applySent.value && !!planId.value && phase.value === 'ready')
const showConfirm = computed(() => phase.value === 'ready' && !!planId.value)
const moveCount = computed(() => Math.max(0, planCount.value - deleteCount.value))
const confirmLabel = computed(() => {
  if (!deleteCount.value) return t('review.confirmMove', { count: formatCount(planCount.value) })
  if (!moveCount.value) return t('review.confirmDelete', { count: formatCount(deleteCount.value) })
  return t('review.confirmMoveDelete', { count: deleteCount.value, moves: formatCount(moveCount.value), deletes: formatCount(deleteCount.value) })
})
/** One box confirms both: the moves, and what is removed for good. */
const checkLabel = computed(() => {
  if (!deleteCount.value) return t('review.confirmCheck')
  const key = moveCount.value ? 'review.confirmCheckDelete' : 'review.confirmCheckDeleteOnly'
  return t(key, { count: deleteCount.value, deletes: formatCount(deleteCount.value) })
})
// The backend owns the cleanup kill switch (SPEC 5.7): every disabled
// message comes from the plan or apply refusal it answered.
const disabledMessage = computed(() => planErrorMessage.value || t('review.disabled.cleanup'))

const applyItems = computed(() => jobProgress.value?.items_total ?? (planCount.value || null))
const applyDone = computed(() => jobProgress.value?.items_done ?? (applyItems.value != null ? 0 : null))
const applyPercent = computed(() => applyDone.value != null && applyItems.value ? applyDone.value / applyItems.value * 100 : undefined)
const applyCounts = computed(() => jobProgress.value
  ? { moved: jobProgress.value.moved, deleted: jobProgress.value.deleted, alreadyAbsent: jobProgress.value.already_absent, partial: jobProgress.value.partial, failed: jobProgress.value.failed, changed: jobProgress.value.changed }
  : null)
/** "45 moved · 1 changed": the moved count, then the other outcomes only when they happened. */
function outcomeText(counts: { moved?: number | null; deleted?: number | null; alreadyAbsent?: number | null; partial?: number | null; failed?: number | null; changed?: number | null }, removal: 'move' | 'delete' | 'mixed' = 'move') {
  // Moved is always said, unless the cleanup deletes items for good and moved none;
  // a cleanup that only deletes says its deleted count, 0 too.
  const deletesOnly = removal === 'delete' && !counts.moved
  const parts = counts.moved || (!counts.deleted && !deletesOnly) ? [t('review.count.moved', { count: formatCount(counts.moved ?? 0) })] : []
  if (deletesOnly && !counts.deleted) parts.push(t('review.count.deleted', { count: formatCount(0) }))
  for (const [key, value] of [['deleted', counts.deleted], ['absent', counts.alreadyAbsent], ['partial', counts.partial], ['failed', counts.failed], ['changed', counts.changed]] as const) {
    if (value) parts.push(t(`review.count.${key}`, { count: formatCount(value) }))
  }
  return joinParts(parts, locale)
}
/** The items DSM left in place, with what stayed. */
const leftInPlace = computed(() => (applyResult.value?.targets ?? []).filter(target => !['moved', 'deleted', 'already_absent'].includes(target.outcome)))
/** Items that did not move: the result lists only the first of them. */
const leftCount = computed(() => {
  const result = applyResult.value
  if (!result) return 0
  return Math.max(leftInPlace.value.length, (result.partial ?? 0) + (result.changed ?? 0) + (result.failed ?? 0))
})
const resultPartial = computed(() => applyResult.value?.completion === 'partial' || activeJobState.value === 'partial')
/** The cleanup ended on a cancel: with its result, or a queued job cancelled before it ran. */
const resultCancelled = computed(() => activeJobState.value === 'cancelled' || applyResult.value?.completion === 'cancelled')
/** How many items the cleanup dealt with, and how many it left as they were, once it ended. */
const handledCounts = computed(() => {
  const result = applyResult.value
  const total = result?.items ?? jobProgress.value?.items_total ?? (planCount.value || 0)
  const done = result
    ? (result.moved ?? 0) + (result.deleted ?? 0) + (result.already_absent ?? 0) + (result.partial ?? 0) + (result.changed ?? 0) + (result.failed ?? 0)
    : jobProgress.value?.items_done ?? 0
  return { done, left: Math.max(0, total - done) }
})
/** Cancel was asked for, but the cleanup had dealt with every item by then. */
const cancelTooLate = computed(() => cancelPending.value && !resultCancelled.value && phase.value === 'result' && !!applyResult.value && handledCounts.value.left === 0)
/** The result's restore hint: only items in a Recycle Bin can be restored. */
const anyMoved = computed(() => !!(applyResult.value?.moved || applyResult.value?.partial))
/** The result's title for a cleanup that finished, by what it did. */
const completeTitle = computed(() => {
  const result = applyResult.value
  if (result?.deleted && !anyMoved.value) return t('review.result.deletedTitle')
  if (result?.deleted) return t('review.result.cleanedTitle')
  return t('review.result.completeTitle')
})

/** What the running cleanup does: by its plan, else (a job followed without it) by what it has done so far. */
const applyingRemoval = computed<'move' | 'delete' | 'mixed'>(() => {
  if (removal.value) return removal.value
  const deleted = jobProgress.value?.deleted ?? 0
  if (!deleted) return 'move'
  return jobProgress.value?.moved || jobProgress.value?.partial ? 'mixed' : 'delete'
})
const APPLYING_TITLE = { move: 'review.applying.title', delete: 'review.applying.titleDelete', mixed: 'review.applying.titleMixed' } as const
/**
 * The job's state in words. A queued duplicates cleanup is checking its
 * kept copies before anything moves: say that, not only "queued".
 */
const applyingLabel = computed(() => {
  const kept = plan.value?.duplicates?.kept ?? 0
  if (activeJobState.value === 'queued' && kept > 0 && !cancelPending.value) return t('review.applying.checkingKept', { count: kept, kept: formatCount(kept) })
  return t(`review.jobState.${activeJobState.value}`)
})
/** A cancel of a running cleanup: the item under way finishes first, the rest stays. */
const finishingCurrent = computed(() => (cancelPending.value || activeJobState.value === 'cancelling') && activeJobState.value !== 'queued' && !!jobProgress.value)

const dialogTitle = computed(() => {
  if (phase.value === 'applying') return t(APPLYING_TITLE[applyingRemoval.value])
  if (['no_report', 'busy', 'disabled', 'refused', 'error', 'apply_error'].includes(phase.value)) return t('review.result.unableToStartTitle')
  if (phase.value === 'result') {
    if (resultCancelled.value) return t('review.result.cancelledTitle')
    if (!applyResult.value) return t(jobReason.value === 'not_started' ? 'common.job.notStarted' : 'common.job.interrupted')
    if (resultPartial.value) return t('review.result.partialTitle')
    if (applyResult.value.completion === 'complete' || activeJobState.value === 'completed') return completeTitle.value
    if (applyResult.value.completion === 'failed' || applyResult.value.completion === 'busy' || activeJobState.value === 'failed') return t('review.result.unableToStartTitle')
    return t('review.result.title')
  }
  return t('review.title')
})
/** Below the title while confirming: what moves, and what emptying the Recycle Bin then frees. */
const dialogDescription = computed(() => {
  if (phase.value !== 'ready' || !planId.value || !moveSize.value) return undefined
  if (deleteCount.value) {
    const deletes = t('review.delete.title', { count: deleteCount.value, deletes: formatCount(deleteCount.value) })
    if (!moveCount.value) return deletes
    const moving = movingShares.value.reduce((sum, share) => sum + share.allocated_bytes, 0)
    return joinParts([t('review.willMoveOnly', { count: formatCount(moveCount.value), bytes: formatBytes(moving) }), deletes], locale)
  }
  const values = { count: formatCount(planCount.value), bytes: formatBytes(moveSize.value.allocated_bytes) }
  return freeLater.value != null
    ? t('review.willMove', { ...values, free: formatBytes(freeLater.value) })
    : t('review.willMoveOnly', values)
})

/** While planning: the step and how far it is, when the job says. */
const planningText = computed(() => {
  const progress = planProgress.value
  if (!progress?.total) return t('review.planning')
  const values = { done: formatCount(progress.done), total: formatCount(progress.total) }
  return t(progress.step === 'kept' ? 'review.planning.kept' : 'review.planning.count', values)
})

function stopPolling() {
  clearTimeout(pollTimer)
  pollTimer = undefined
}

function localizedApiError(error: unknown) {
  if (error instanceof ApiError) return t(`review.apiError.${error.code}`)
  return t('review.apiError.unknown')
}

/** Why one item is protected, as the plan said it. */
function protectedMessage(error: Pick<CleanupError, 'reason' | 'package'>) {
  if (error.reason === 'package' && error.package) return t('review.protected.packageNamed', { package: error.package })
  return t(`review.protected.${error.reason ?? 'other'}`)
}
/** What the plan says about one refused item. */
function itemMessage(error: CleanupError) {
  return error.code === 'protected_target' ? protectedMessage(error) : refusalMessage(error.code)
}

function refusalMessage(code: string) {
  const keys: Record<string, string> = {
    cleanup_disabled: 'cleanupDisabled',
    busy: 'busy',
    no_report: 'noReport',
    target_changed: 'targetChanged',
    not_found: 'notFound',
    plan_stale: 'changed',
    cancelled: 'cancelled',
    in_recycle_bin: 'inRecycleBin',
    selection_boundary: 'boundary',
    outside_allowed_roots: 'outsideRoots',
    recycle_bin_unavailable: 'recycleBinUnavailable',
    untrusted_state: 'untrustedState',
    io_error: 'ioError',
    duplicates_stale: 'duplicatesStale',
    duplicates_unavailable: 'duplicatesStale',
    duplicates_invalid: 'duplicatesStale',
    duplicates_unknown_group: 'duplicatesUnknownGroup',
    keeper_changed: 'keeperChanged',
    keeper_removed: 'keeperRemoved',
    keeper_conflict: 'keeperConflict',
    duplicates_nothing: 'duplicatesNothing',
    delete_not_confirmed: 'deleteNotConfirmed',
  }
  if (code === 'protected_target') return t('review.protected.other')
  return t(`review.refusal.${keys[code] ?? 'other'}`)
}

/** Refusals about one selected item: removing it from the list lets the rest go ahead. */
const REMOVABLE_REFUSALS = new Set(['target_changed', 'not_found', 'selection_boundary', 'protected_target', 'in_recycle_bin', 'outside_allowed_roots'])
function canDrop(error: CleanupError) {
  if (!REMOVABLE_REFUSALS.has(error.code)) return false
  if (error.group) return !!duplicateSelection.value?.groups.some(group => group.id === error.group)
  return selectedItems.value.some(item => item.path === error.path)
}
function errorPath(error: CleanupError) {
  return selectedItems.value.find(item => item.path === error.path)?.path ?? error.path ?? t('review.refusal.selectedItem')
}
/** Takes the refused item out of the list: a duplicates copy goes with its group. */
function dropItem(error: CleanupError) {
  if (error.group) basket.value?.removeDuplicates([error.group])
  else basket.value?.remove(error)
}
const outcomeLabel = (outcome: string) => t(`review.outcome.${['partial', 'changed', 'failed'].includes(outcome) ? outcome : 'failed'}`)

/** A plan's refusal: a keeper conflict names the copy and both groups when the CLI says which. */
function planRefusalMessage(code: string, error?: Partial<Pick<CleanupError, 'path' | 'conflict'>>) {
  const conflict = error?.conflict
  if (code === 'keeper_conflict' && conflict) {
    return t('review.refusal.keeperConflictNamed', { copy: error.path ?? t('review.refusal.selectedItem'), kept: conflict.kept_in.name.path, removing: conflict.removed_by.name.path })
  }
  return refusalMessage(code)
}

function showPlanRefusal(response: PlanResponse) {
  const refusal = response.errors?.[0]?.code ?? response.completion ?? 'unknown'
  const conflicts = (response.errors ?? []).filter(error => error.code === 'keeper_conflict' && error.conflict).length
  planErrorMessage.value = refusal === 'keeper_conflict' && conflicts > 1
    ? t('review.refusal.keeperConflicts', { count: conflicts, groups: formatCount(conflicts) })
    : planRefusalMessage(refusal, response.errors?.[0])
  if (refusal === 'cleanup_disabled') phase.value = 'disabled'
  else if (refusal === 'cancelled') phase.value = 'stopped'
  else if (refusal === 'busy') phase.value = 'busy'
  else if (refusal === 'no_report') phase.value = 'no_report'
  else if (response.completion === 'failed' || response.completion === 'busy') phase.value = 'refused'
  else phase.value = 'error'
}

function reset() {
  stopPolling()
  applySent.value = false
  submittedPlanId = null
  knownJobIds = []
  plan.value = null
  planErrorMessage.value = ''
  applyErrorMessage.value = ''
  applyResult.value = null
  applyRefusal.value = null
  activeJobId.value = null
  jobReason.value = null
  jobProgress.value = null
  pollError.value = false
  planProgress.value = null
  planJobId.value = null
  stoppingPlan.value = false
  pollFailures = 0
  cancelPending.value = false
  cancelWanted.value = false
  cancelError.value = false
}

async function loadPlan() {
  planningController.abort()
  planningController = new AbortController()
  if (!open.value || phase.value === 'applying') return
  const requestId = ++planRequest.value
  reset()
  if (!selectedItems.value.length && !duplicateSelection.value?.groups.length) {
    phase.value = 'empty'
    return
  }
  phase.value = 'planning'
  try {
    if (!(await authorizeRootWork())) {
      if (requestId === planRequest.value) open.value = false
      return
    }
    // Preserve the identity the user selected from the report. A large
    // selection is staged chunk by chunk first: no request holds it whole.
    const signal = planningController.signal
    const params = await planParams(selectedItems.value, volumePath.value ?? '', duplicateSelection.value, chunk => api('cleanup.select', chunk), signal)
    if (requestId !== planRequest.value || !open.value) return
    const response = await runInBackground('cleanup.plan', params, {
      signal,
      onAccepted: id => { if (requestId === planRequest.value) planJobId.value = id },
      onProgress: progress => { if (requestId === planRequest.value) planProgress.value = progress },
    }) as PlanResponse
    if (requestId !== planRequest.value || !open.value) return
    plan.value = response
    if (resolving.value) {
      // More conflicts than the last plan named (an older CLI names only the
      // first): take those groups out too, unless the user must pick.
      const next = conflictResolution((response.errors ?? []).filter(error => error.code === 'keeper_conflict'), listedGroups.value)
      if (next.drop.length && !next.choices.length && ++resolveRounds < MAX_RESOLVE_ROUNDS) {
        dropConflictGroups(next.drop)
        return
      }
      resolving.value = false
    }
    if (response.shares?.length && response.plan_id) {
      phase.value = 'ready'
      return
    }
    if (response.completion !== 'complete' || !response.plan_id) {
      showPlanRefusal(response)
      return
    }
    planErrorMessage.value = t('review.apiError.unknown')
    phase.value = 'error'
  } catch (error) {
    if (requestId !== planRequest.value) return
    planErrorMessage.value = localizedApiError(error)
    phase.value = 'error'
  }
}

/**
 * Stops the plan's check: the job is cancelled on the NAS and the dialog
 * stops following it at once, so the user can recheck or close. A job that
 * already ended has nothing to stop.
 */
async function stopPlan() {
  const id = planJobId.value
  if (!id || stoppingPlan.value) return
  stoppingPlan.value = true
  planRequest.value++
  planningController.abort()
  phase.value = 'stopped'
  try {
    await api('job.cancel', { job_id: id })
  } catch { /* The job may have ended already: nothing is left to stop. */ }
  finally {
    stoppingPlan.value = false
  }
}

function pathKey(path: CleanupPath) {
  return path.path_hex ?? `utf8:${path.path}`
}

/**
 * Takes what the cleanup dealt with (moved, deleted for good or already
 * gone) out of the cleanup list. The result lists only what stayed, so every
 * item of a folder that was cleaned goes except those; when that list is cut
 * short, or nothing went, the list stays as it is for a recheck.
 */
function removeMovedBasketItems(result: CleanupResult) {
  // Items deleted for good are dealt with too: they are gone.
  const moved = (result.moved ?? 0) + (result.deleted ?? 0) + (result.already_absent ?? 0) + (result.partial ?? 0)
  if (!moved || leftInPlace.value.length < leftCount.value) return
  const stayed = new Set(leftInPlace.value.map(target => pathKey(target.path)))
  const cleaned = shares.value.map(share => share.path)
  for (const item of selectedItems.value) {
    if (!stayed.has(pathKey(item)) && cleaned.some(share => basket.value?.isWithin(item, share))) basket.value?.remove(item)
  }
  // The groups went with the plan as a whole: once a copy moved their
  // analysis no longer holds, so Duplicates must be looked at again.
  if (duplicateSelection.value?.groups.length) basket.value?.clearDuplicates()
}

function finishApply(result: CleanupResult | null, terminalState?: string) {
  applyResult.value = result
  if (result && !props.jobId) removeMovedBasketItems(result)
  const refusal = result?.errors?.[0]?.code
  if (result && (result.completion === 'failed' || result.completion === 'busy') && refusal && DUPLICATE_REFUSALS.has(refusal)) {
    // A kept copy went or changed size after the plan: nothing moved.
    applyRefusal.value = refusal
    planErrorMessage.value = planRefusalMessage(refusal, result.errors?.[0])
    phase.value = 'refused'
    stopPolling()
    return
  }
  if (refusal === 'cleanup_disabled') {
    planErrorMessage.value = refusalMessage(refusal)
    phase.value = 'disabled'
    stopPolling()
    return
  }
  if (result?.completion === 'failed' || result?.completion === 'busy') {
    applyErrorMessage.value = refusalMessage(refusal ?? result.completion)
  }
  if (terminalState) activeJobState.value = terminalState
  phase.value = 'result'
  // The move changed what the reports describe: pages must read again.
  invalidateAnswers()
  stopPolling()
  // The page's job strip reads jobs.list on its own, slower cadence: read it now.
  pollNow()
}

async function pollApplyJob(jobId: string) {
  pollTimer = undefined
  try {
    const response = await api('jobs.list', {})
    const job = (response.jobs as unknown as CleanupJob[]).find(candidate => candidate.job_id === jobId && candidate.kind === 'cleanup-apply' && candidate.phase === 'execute')
    if (disposed || !open.value || phase.value !== 'applying' || activeJobId.value !== jobId) return
    if (!job) {
      finishApply(null, 'interrupted')
      return
    }
    activeJobState.value = job.state
    jobProgress.value = job.cleanup ?? null
    jobReason.value = job.reason ?? null
    pollError.value = false
    pollFailures = 0
    if (!['queued', 'running', 'pausing', 'cancelling'].includes(job.state)) {
      let result: CleanupResult | null = null
      try {
        const document = await api('job.result', { job_id: jobId }) as CleanupResult
        if (document.command === 'cleanup-apply') result = document
      } catch { /* No final document was published. */ }
      if (!disposed && open.value && activeJobId.value === jobId) finishApply(result, job.state)
      return
    }
  } catch {
    // jobs.list is read-only; keep polling, but never repeat cleanup.apply.
    if (disposed || activeJobId.value !== jobId) return
    pollError.value = true
    if (++pollFailures >= 3) {
      applyErrorMessage.value = t('review.applyError.noRetry')
      phase.value = 'apply_error'
      stopPolling()
      return
    }
  }
  if (!disposed && phase.value === 'applying' && activeJobId.value === jobId) {
    pollTimer = setTimeout(() => void pollApplyJob(jobId), 2000)
  }
}

/** Cancel was asked for before cleanup.apply named its job: it is sent once the job is known. */
const cancelWanted = ref(false)
async function cancelApply() {
  if (cancelPending.value) return
  if (!activeJobId.value) {
    cancelWanted.value = true
    cancelPending.value = true
    return
  }
  cancelWanted.value = false
  cancelPending.value = true
  cancelError.value = false
  try {
    if (!(await authorizeRootWork())) {
      cancelPending.value = false
      return
    }
    const response = await api('job.cancel', { job_id: activeJobId.value })
    if (response.outcome === 'requested' || response.outcome === 'cancelled') {
      if (response.state) activeJobState.value = response.state
      stopPolling()
      void pollApplyJob(activeJobId.value)
    } else {
      cancelPending.value = false
      cancelError.value = true
      if (response.state) activeJobState.value = response.state
    }
  } catch {
    cancelPending.value = false
    cancelError.value = true
  }
}

async function applyCleanup() {
  if (!canApply.value || !planId.value) return
  applySent.value = true
  if (!(await authorizeRootWork())) {
    applySent.value = false
    return
  }
  const id = planId.value
  phase.value = 'applying'
  activeJobState.value = 'queued'
  jobProgress.value = null
  pollError.value = false
  cancelPending.value = false
  cancelWanted.value = false
  cancelError.value = false
  try {
    // The WebAPI maps this sealed-plan request to cleanup apply --detach=none;
    // apply is never retried automatically.
    knownJobIds = (await api('jobs.list', {})).jobs.map(job => job.job_id)
    if (disposed) return
    submittedPlanId = id
    const response = await api('cleanup.apply', cleanupApplyParams(id, deleteCount.value > 0))
    if (response.command === 'detach' && response.job_id) {
      activeJobId.value = response.job_id
      activeJobState.value = 'queued'
      pollNow()
      if (cancelWanted.value) {
        cancelPending.value = false
        await cancelApply()
        return
      }
      void pollApplyJob(response.job_id)
      return
    }
    if (response.command === 'cleanup-apply') {
      finishApply(response as CleanupResult, response.completion === 'failed' || response.completion === 'busy' ? 'failed' : undefined)
      return
    }
    if (!await recoverApply()) finishApply(null)
  } catch (error) {
    if (submittedPlanId && await recoverApply()) return
    applyErrorMessage.value = localizedApiError(error)
    phase.value = 'apply_error'
    stopPolling()
  }
}

async function recoverApply() {
  try {
    const response = await api('jobs.list', {})
    if (disposed) return false
    const job = findSubmittedJob(response.jobs, 'cleanup-apply', submittedPlanId!, knownJobIds)
    if (!job) return false
    activeJobId.value = job.job_id
    phase.value = 'applying'
    if (cancelWanted.value) {
      cancelPending.value = false
      await cancelApply()
      return true
    }
    void pollApplyJob(job.job_id)
    return true
  } catch { return false }
}

function startRescan() {
  open.value = false
  openNewScan()
}

const allowing = ref(false)
const allowFailed = ref(false)

/** Turns the cleanup kill switch back off (setup.cleanup), then plans again. */
async function allowCleanup() {
  allowing.value = true
  allowFailed.value = false
  try {
    if (!(await authorizeRootWork())) return
    const { config } = await api('setup.cleanup', { mutations_disabled: false })
    // A refused `config set` prints no config: that is a failure, not a no-op.
    if (!config) throw new ApiError('unknown')
    session.cleanupEnabled = config.cleanup_enabled
    session.mutationsDisabled = config.mutations_disabled
    void loadPlan()
  } catch {
    allowFailed.value = true
  } finally {
    allowing.value = false
  }
}

let wasOpen = false
watch([open, volumePath, selectedSignature, () => props.jobId], ([isOpen]) => {
  if (isOpen) {
    wasOpen = true
    if (props.jobId && activeJobId.value !== props.jobId) {
      stopPolling()
      jobProgress.value = null
      cancelPending.value = false
      cancelError.value = false
      activeJobId.value = props.jobId
      phase.value = 'applying'
      void pollApplyJob(props.jobId)
      return
    }
    // A cleanup that ran on while the dialog was closed: follow its job
    // again, so the dialog shows where it is now, or how it ended.
    if (phase.value === 'applying') {
      if (activeJobId.value) {
        stopPolling()
        void pollApplyJob(activeJobId.value)
      }
      return
    }
    if (props.jobId) return
    if (phase.value === 'result' || phase.value === 'apply_error') return
    void loadPlan()
  } else if (wasOpen) {
    planningController.abort()
    wasOpen = false
    resolving.value = false
    droppedGroups.value = []
    picked.value = {}
    // A finished cleanup is done with once closed: the next open plans the
    // list as it is then. A running move keeps its job (the job strip shows it).
    if (phase.value === 'result' || phase.value === 'apply_error') {
      reset()
      phase.value = 'idle'
    }
    emit('close')
  }
}, { immediate: true })

onBeforeUnmount(() => {
  planningController.abort()
  disposed = true
  planRequest.value++
  stopPolling()
})
</script>

<template>
  <ConfirmDialog
    v-model:open="open"
    :title="dialogTitle"
    :description="dialogDescription"
    :check-label="checkLabel"
    :confirm-label="confirmLabel"
    :cancel-label="t(phase === 'planning' || phase === 'ready' ? 'common.cancel' : 'common.close')"
    :close-label="t('common.close')"
    :disabled="!canApply"
    :loading="phase === 'applying'"
    :show-confirm="showConfirm"
    @confirm="applyCleanup"
  >
    <div class="review-dialog">
      <LoadingArea
        v-if="phase === 'planning'"
        :text="planningText"
        data-test="review-planning"
      />

      <Callout v-else-if="phase === 'stopped'" :dismissible="false" tone="info" :title="t('review.refusal.cancelled')" data-test="review-stopped" />

      <Callout v-else-if="phase === 'empty'" tone="info" :title="t('review.empty.title')">
        <p>{{ t('review.empty') }}</p>
      </Callout>

      <Callout v-else-if="phase === 'disabled'" :dismissible="false" tone="warning" :title="disabledMessage">
        <p v-if="allowFailed">{{ t('review.disabled.allowFailed') }}</p>
      </Callout>

      <StateView v-else-if="phase === 'no_report'" kind="no_report" />

      <Callout v-else-if="phase === 'busy'" :dismissible="false" tone="warning" :title="t('review.refusal.busy')" />

      <template v-else-if="phase === 'refused'">
        <Callout :dismissible="false" tone="warning" :title="planErrorMessage" />
        <ul v-if="itemErrors.length" class="review-dialog__refused">
          <li v-for="error in shownErrors" :key="error.path + error.code">
            <span class="review-dialog__refused-text" data-test="review-refused-item">{{ errorPath(error) }}<small> · {{ itemMessage(error) }}</small></span>
            <Button v-if="canDrop(error)" class="review-dialog__refused-drop" data-test="drop-changed" size="sm" variant="secondary" @click="dropItem(error)">{{ t('review.dropChanged') }}</Button>
          </li>
        </ul>
        <p v-if="itemErrors.length > shownErrors.length" class="review-dialog__note" data-test="review-more-refused">{{ t('review.more', { count: formatCount(itemErrors.length - shownErrors.length) }) }}</p>
        <ul v-if="keeperConflicts.length > 1" class="review-dialog__refused" data-test="review-conflicts">
          <li v-for="error in keeperConflicts.slice(0, SHOWN)" :key="error.conflict!.kept_in.id + error.conflict!.removed_by.id">
            <span class="review-dialog__refused-text" data-test="review-conflict">{{ t('review.refusal.keeperConflictLine', { removing: error.conflict!.removed_by.name.path, copy: error.path, kept: error.conflict!.kept_in.name.path }) }}</span>
          </li>
        </ul>
        <section v-for="pair in resolution.choices" :key="pairKey(pair)" class="review-dialog__share" data-test="review-conflict-choice">
          <p>{{ t('review.duplicates.conflictChoice', { first: pair[0].name, second: pair[1].name }) }}</p>
          <div class="review-dialog__choice">
            <Button
              v-for="group in pair"
              :key="group.id"
              size="sm"
              :variant="picked[pairKey(pair)] === group.id ? 'primary' : 'secondary'"
              :aria-pressed="picked[pairKey(pair)] === group.id"
              data-test="review-conflict-pick"
              @click="pickConflictGroup(pair, group)"
            >
              {{ t('review.duplicates.dropConflict', { group: group.name }) }}
            </Button>
          </div>
        </section>
      </template>

      <Callout v-else-if="phase === 'error'" :dismissible="false" tone="danger" :title="planErrorMessage" />

      <template v-else-if="phase === 'ready'">
        <Callout v-if="droppedGroups.length" :dismissible="false" tone="info" :title="t('review.duplicates.droppedConflicts', { count: droppedGroups.length, groups: formatCount(droppedGroups.length) })" data-test="review-dropped-conflicts">
          <ul class="review-dialog__dropped">
            <li v-for="group in droppedGroups" :key="group.id">{{ group.name }}</li>
          </ul>
        </Callout>
        <Callout v-if="planId && deleteCount" :dismissible="false" tone="danger" :title="t('review.delete.title', { count: deleteCount, deletes: formatCount(deleteCount) })" data-test="review-delete">
          <p>{{ t('review.delete.body') }}</p>
        </Callout>
        <Callout v-if="planId && moveCount" :dismissible="false" tone="info" :title="t('review.recycle.title')" data-test="review-recycle">
          <p>{{ t('review.recycle.body') }}</p>
          <p data-test="review-time">{{ t('review.recycle.time') }}</p>
          <p v-if="binSymlinks" data-test="review-symlinks">{{ t('review.recycle.symlinks', { count: binSymlinks, symlinks: formatCount(binSymlinks) }) }}</p>
        </Callout>

        <section v-if="planId && moveSize" class="review-dialog__stats" :aria-label="t('review.numbers.title')">
          <dl>
            <div><dt>{{ t(deleteCount ? 'review.numbers.removes' : 'review.numbers.moves') }}</dt><dd data-test="review-move-size">{{ formatBytes(moveSize.allocated_bytes) }}</dd></div>
            <div><dt>{{ t('review.numbers.items') }}</dt><dd data-test="review-count">{{ formatCount(planCount) }}</dd></div>
            <div v-if="freeLater != null"><dt>{{ t(removal === 'delete' ? 'review.numbers.frees' : 'review.numbers.freeLater') }}</dt><dd data-test="review-free-later">{{ formatBytes(freeLater) }}</dd></div>
          </dl>
          <span class="review-dialog__about"><InfoTip data-test="review-plan-tip" :aria-label="t('review.numbers.about')">
            <ul class="review-dialog__tip">
              <li>{{ t(moveCount ? 'review.numbers.allocated' : 'review.numbers.allocatedDelete') }}</li>
              <template v-if="moveCount">
                <li>{{ t('review.recycle.restore') }}</li>
                <li>{{ t('review.recycle.olderDsm') }}</li>
              </template>
              <li>{{ t(moveCount ? 'review.recycle.sync' : 'review.recycle.syncDelete') }}</li>
            </ul>
          </InfoTip></span>
        </section>

        <template v-for="share in shares" :key="share.path.path_hex ?? share.path.path">
          <section
            class="review-dialog__share"
            :data-test="share.delete ? 'review-share-delete' : 'review-share'"
          >
            <h3>
              {{ t(share.delete_reason === 'no_shared_folder' || (share.delete && !share.delete_reason) ? 'review.delete.folder' : 'review.share.title', { share: share.share.path }) }}
              <small>{{ t('review.share.size', { count: formatCount(shareCount(share)), bytes: formatBytes(share.allocated_bytes) }) }}</small>
            </h3>
            <Callout v-if="share.delete_reason === 'recycle_bin_off'" :dismissible="false" tone="danger" :title="t('review.delete.binOff', { share: share.share.path })" data-test="review-delete-bin-off">
              <p>{{ t('review.delete.binOffEnable') }}</p>
              <template #actions><ShareSettingsButton :share="share.share.path" /></template>
            </Callout>
            <Callout v-else-if="share.delete" :dismissible="false" tone="danger" :title="t('review.delete.share', { share: share.share.path })" data-test="review-delete-share" />
            <p v-else-if="share.recycle_bin.admin_only" class="review-dialog__note" data-test="review-admin-only">{{ t('review.share.adminOnly') }}</p>
            <Callout v-else :dismissible="false" tone="warning" :title="t('review.share.everyone', { share: share.share.path })" data-test="review-everyone" />
            <ul class="review-dialog__paths">
              <li v-for="item in share.items" :key="item.path.path_hex ?? item.path.path">
                <span>{{ item.path.path }}</span>
                <span class="review-dialog__item-meta">
                  <Badge v-if="item.drive" tone="warning" data-test="review-drive-item">{{ t('review.share.driveBadge') }}</Badge>
                  <small>{{ formatBytes(item.allocated_bytes) }}</small>
                </span>
              </li>
            </ul>
            <p v-if="shareCount(share) > share.items.length" class="review-dialog__note" data-test="review-more">{{ t('review.more', { count: formatCount(shareCount(share) - share.items.length) }) }}</p>
          </section>
        </template>
        <p v-if="touchesDrive" class="review-dialog__note" data-test="review-drive">{{ t('review.share.drive') }}</p>

        <section v-if="duplicateSummary.lines.length" class="review-dialog__share" data-test="review-duplicates">
          <h3>{{ t('review.duplicates.title') }}</h3>
          <KeyValueRow
            v-for="line in shownDuplicateLines"
            :key="line.group"
            :label="line.name"
            :value="duplicateLine(line)"
            label-width="40%"
          />
          <p v-if="duplicateSummary.lines.length > shownDuplicateLines.length" class="review-dialog__note">{{ t('review.more', { count: formatCount(duplicateSummary.lines.length - shownDuplicateLines.length) }) }}</p>
          <p v-if="duplicatesScanWarning" class="review-dialog__note" data-test="review-duplicates-scan-age">{{ duplicatesScanWarning }}</p>
          <p v-if="duplicateSummary.partial" class="review-dialog__note" data-test="review-partial">{{ t('review.duplicates.partialNote') }}</p>
          <p v-if="plan?.duplicates?.kept" class="review-dialog__note" data-test="review-kept-checked">{{ t('review.duplicates.keptChecked', { count: plan.duplicates.kept }) }}</p>
          <p v-if="duplicateSummary.freesLater" class="review-dialog__note" data-test="review-frees-later">{{ t('review.duplicates.freesLater', { size: formatBytes(duplicateSummary.freesLater) }) }}</p>
        </section>
      </template>

      <template v-else-if="phase === 'applying'">
        <CellStatus
          tone="info"
          :label="applyingLabel"
          :progress="applyPercent"
          :detail="applyDone != null && applyItems != null
            ? t('review.applying.count', { done: formatCount(applyDone), total: formatCount(applyItems) })
            : t('review.applying.indeterminate')"
        />
        <p v-if="applyCounts" class="review-dialog__counts" data-test="review-applying-counts">{{ outcomeText(applyCounts, applyingRemoval) }}</p>
        <p v-if="finishingCurrent" class="review-dialog__note" data-test="review-finishing">{{ t('review.applying.finishingCurrent') }}</p>
        <p v-if="jobProgress?.calls_done" class="review-dialog__counts" data-test="review-calls">{{ joinParts([t('review.applying.calls', { count: formatCount(jobProgress.calls_done) }), formatBytes(jobProgress.bytes_moved)], locale) }}</p>
        <p v-if="applyingRemoval !== 'delete'" class="review-dialog__note">{{ t('review.recycle.time') }}</p>
        <Callout v-if="cancelError" tone="warning" :title="t('review.applying.cancelError')" />
        <Callout v-else-if="pollError" tone="warning" :title="t('review.applying.pollError')" />
      </template>

      <template v-else-if="phase === 'result'">
        <Callout v-if="applyErrorMessage" :dismissible="false" tone="danger" :title="applyErrorMessage" />
        <p v-if="resultCancelled" class="review-dialog__counts" data-test="review-cancelled-counts">{{ t('review.result.cancelledCounts', { done: formatCount(handledCounts.done), left: formatCount(handledCounts.left), count: handledCounts.left }) }}</p>
        <p v-if="cancelTooLate" class="review-dialog__note" data-test="review-cancel-too-late">{{ t('review.result.cancelTooLate') }}</p>
        <p v-if="!applyResult && applyCounts && !resultCancelled" class="review-dialog__counts">{{ t('review.result.lastKnown', { counts: outcomeText(applyCounts) }) }}</p>
        <p v-else-if="!applyResult && !resultCancelled">{{ t('review.result.detailsUnavailable') }}</p>
        <template v-if="applyResult && !applyErrorMessage">
          <Callout v-if="applyResult.deleted" :dismissible="false" tone="success" :title="t('review.result.deleted', { count: applyResult.deleted, deleted: formatCount(applyResult.deleted), bytes: formatBytes(applyResult.deleted_bytes ?? 0) })" data-test="review-deleted" />
          <Callout v-if="anyMoved" :dismissible="false" tone="success" :title="t('review.result.moved', { bytes: formatBytes(applyResult.moved_bytes ?? 0) })" data-test="review-moved">
            <p>{{ t('review.result.empty') }}</p>
          </Callout>
          <p class="review-dialog__counts">{{ outcomeText({ moved: applyResult.moved, deleted: applyResult.deleted, alreadyAbsent: applyResult.already_absent, partial: applyResult.partial, failed: applyResult.failed, changed: applyResult.changed }) }}</p>
          <ul v-if="applyResult.recycle_bins?.length" class="review-dialog__paths" data-test="review-bins">
            <li v-for="bin in applyResult.recycle_bins" :key="bin.path.path_hex ?? bin.path.path">
              <span>{{ t('review.result.bin', { share: bin.share.path, count: formatCount(bin.items) }) }}</span>
              <small>{{ formatBytes(bin.moved_bytes) }}</small>
            </li>
          </ul>
          <p v-if="anyMoved" class="review-dialog__note">{{ t('review.recycle.restore') }}</p>
        </template>
        <section v-if="leftInPlace.length" class="review-dialog__share" data-test="review-left">
          <h3>{{ t('review.result.leftTitle') }}</h3>
          <ul class="review-dialog__paths">
            <li v-for="target in leftInPlace" :key="target.path.path_hex ?? target.path.path" :title="target.detail ?? undefined">
              <span>{{ target.path.path }}</span><small>{{ outcomeLabel(target.outcome) }}</small>
            </li>
          </ul>
          <p v-if="leftCount > leftInPlace.length" class="review-dialog__note">{{ t('review.more', { count: formatCount(leftCount - leftInPlace.length) }) }}</p>
        </section>
      </template>

      <Callout v-else-if="phase === 'apply_error'" :dismissible="false" tone="danger" :title="applyErrorMessage">
        <p v-if="applyErrorMessage !== t('review.applyError.noRetry')">{{ t('review.applyError.noRetry') }}</p>
      </Callout>
    </div>

    <template #actions>
      <Button v-if="binOffShares.length" variant="secondary" data-test="review-recheck" @click="loadPlan">{{ t('review.recheck') }}</Button>
    </template>
    <template #footer>
      <div class="review-dialog__footer">
        <Button variant="secondary" @click="open = false">{{ t('common.close') }}</Button>
        <Button v-if="phase === 'planning' && planJobId" variant="secondary" :loading="stoppingPlan" data-test="review-stop-plan" @click="stopPlan">{{ t('review.planning.stop') }}</Button>
        <Button v-if="phase === 'disabled'" variant="primary" :loading="allowing" @click="allowCleanup">{{ t('review.disabled.allow') }}</Button>
        <Button v-else-if="phase === 'no_report'" variant="primary" @click="startRescan">{{ t('state.noReport.scan') }}</Button>
        <Button v-else-if="phase === 'result'" variant="primary" @click="startRescan">{{ t('review.rescan.open') }}</Button>
        <Button v-if="phase === 'refused' && refusalCodes.some(code => RESCAN_REFUSALS.includes(code))" data-test="review-rescan" variant="primary" @click="startRescan">{{ t('review.rescan.open') }}</Button>
        <Button v-if="canResolveConflicts" data-test="review-resolve-conflicts" variant="primary" :disabled="!allPicked" @click="resolveConflicts">{{ resolveLabel }}</Button>
        <Button v-if="canClearDuplicates" data-test="review-clear-duplicates" variant="secondary" @click="basket?.clearDuplicates()">{{ t('review.duplicates.clear') }}</Button>
        <Button v-if="phase === 'apply_error' && submittedPlanId" @click="recoverApply">{{ t('newScan.busy.view') }}</Button>
        <Button v-else-if="!props.jobId && (phase === 'refused' || phase === 'stopped' || phase === 'busy' || (phase === 'result' && !applyResult))" variant="secondary" data-test="review-recheck" @click="loadPlan">{{ t('review.recheck') }}</Button>
        <Button v-if="phase === 'applying'" variant="secondary" :loading="cancelPending" data-test="review-cancel" @click="cancelApply">{{ t(cancelPending ? 'review.applying.cancelling' : 'review.applying.cancel') }}</Button>
      </div>
    </template>
  </ConfirmDialog>
</template>

<style scoped>
.review-dialog {
  display: grid;
  gap: 16px;
  padding: 2px;
}

.review-dialog h3 {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  margin: 0 0 8px;
  font-size: var(--ui-font-size);
}

.review-dialog h3 small { color: var(--ui-text-secondary); font-weight: normal; }
.review-dialog p { margin: 0; }
.review-dialog p + p { margin-top: 8px; }
.review-dialog__note { margin: 0; color: var(--ui-text-secondary); font-size: 12px; }
.review-dialog__stats,
.review-dialog__share {
  display: grid;
  gap: 8px;
  border: 1px solid var(--ui-border);
  border-radius: var(--ui-radius-md);
  padding: 12px;
}

.review-dialog__share > .ui-button,
.review-dialog__share > button { justify-self: start; }

.review-dialog__stats dl {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
  gap: 10px 16px;
  padding-right: 24px;
  margin: 0;
}

.review-dialog__stats dl div { min-width: 0; }
.review-dialog__stats dt { color: var(--ui-text-secondary); font-size: 0.875rem; }
.review-dialog__stats dd { margin: 2px 0 0; font-size: 1rem; font-weight: 600; overflow-wrap: anywhere; }
.review-dialog__tip { max-width: 320px; margin: 0; padding-left: 16px; }
.review-dialog__tip li + li { margin-top: 4px; }
.review-dialog__stats { position: relative; }
.review-dialog__about { position: absolute; top: 8px; right: 8px; }
.review-dialog__counts { color: var(--ui-text-secondary); }
.review-dialog__refused { display: grid; gap: 6px; margin: 0; padding: 0; list-style: none; }
.review-dialog__refused li { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
/* The text wraps; the button keeps its whole label. */
.review-dialog__refused-text { flex: 1 1 auto; min-width: 0; overflow-wrap: anywhere; }
.review-dialog__refused-drop { flex: 0 0 auto; }
.review-dialog__refused small { color: var(--ui-text-secondary); }
.review-dialog__choice { display: flex; flex-wrap: wrap; gap: 8px; }
.review-dialog__dropped { margin: 0; padding-left: 16px; overflow-wrap: anywhere; }

.review-dialog__paths {
  display: grid;
  gap: 6px;
  max-height: 240px;
  margin: 0;
  padding: 0;
  overflow: auto;
  list-style: none;
}

.review-dialog__paths li { display: flex; justify-content: space-between; gap: 12px; }
.review-dialog__paths span { min-width: 0; overflow-wrap: anywhere; }
.review-dialog__paths small { flex: 0 0 auto; color: var(--ui-text-secondary); }
.review-dialog__item-meta { display: inline-flex; flex: 0 0 auto; align-items: center; gap: 6px; }
.review-dialog__footer { display: flex; justify-content: flex-end; gap: 8px; width: 100%; }
</style>
