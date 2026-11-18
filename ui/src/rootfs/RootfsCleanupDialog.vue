<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import ReclaimGrid from '@/components/ReclaimGrid.vue'
import { Button, Callout, Checkbox, Dialog, ExpandableItem, Statistic, Surface } from '@synopackageland/ui'
import { ApiError, api } from '@/api'
import { runInBackground } from '@/api/planning'
import { authorizeRootWork } from '@/stores/authorization'
import type { Job, Path, Volume } from '@/types'
import { formatBytes, formatCount, formatVolume, joinParts, joinSentences, locale, t } from '@/i18n'
import RootfsApplyStep from '@/pages/RootfsApply/RootfsApplyStep.vue'
import OptionPicker from '@/components/OptionPicker.vue'
import type { RootfsApplyResult } from '@/pages/RootfsApply/rootfsTypes'
import {
  scanningVolumeMounts,
  defaultDestination,
  refusalMessageKey,
  shareReasons,
  splitSmallMoveItems,
  type CleanupShare,
  type ShareBlockReason,
} from './cleanupModel'
import type { RootfsReportDocument } from '@/types'
import type { RootfsPlanDocument } from './cleanupTypes'

type Stage = 'destination' | 'review' | 'apply' | 'normal'
type Failure = { key: string; sharePath?: string }

const emit = defineEmits<{
  complete: [result: RootfsApplyResult]
  refresh: []
}>()
const isOpen = ref(false)
const stage = ref<Stage>('destination')
const loading = ref(false)
const planning = ref(false)
const applying = ref(false)
const mode = ref<'standard' | 'deep'>('standard')
const deepChecked = ref(false)
const modeOptions = computed(() => (['standard', 'deep'] as const).map(value => ({
  value, label: t(`rootfsClean.mode.${value}`), meta: t(`rootfsClean.mode.${value}Body`),
})))
function abandonPreview() {
  planningController.abort()
  planningController = new AbortController()
  planning.value = false
  plan.value = null
}
function chooseMode(values: string[]) {
  if (planning.value) abandonPreview()
  mode.value = values[0] === 'deep' ? 'deep' : 'standard'
  checked.value = false
  deepChecked.value = false
}
const report = ref<RootfsReportDocument | null>(null)
const plan = ref<RootfsPlanDocument | null>(null)
const volumeDocument = ref<{ volumes: Volume[] } | null>(null)
const jobs = ref<Job[]>([])
const selectedPath = ref<string | null>(null)
const checked = ref(false)
const failure = ref<Failure | null>(null)
const serverBusyPath = ref<string | null>(null)

// rootfs plan takes a shared folder on any data volume, Btrfs or ext4 (SPEC 5.23).
const shares = computed<CleanupShare[]>(() =>
  (volumeDocument.value?.volumes ?? []).flatMap(volume =>
    volume.shares.map(share => ({
      name: pathValue(share.name),
      path: pathValue(share.path),
      mount: pathValue(volume.mount),
      freeBytes: volume.free_bytes ?? 0,
    })),
  ),
)
const scanningMounts = computed(() => scanningVolumeMounts(jobs.value))
// Exact mutation path conflicts are checked by the root executor, never inferred from volume activity.
// Deep mode moves more, so it needs the room its own totals name (a report from an older CLI has none).
const requiredBytes = computed(() => {
  const cleanable = report.value?.cleanable
  return (mode.value === 'deep' ? cleanable?.deep?.required_destination_bytes : undefined) ?? cleanable?.required_destination_bytes ?? 0
})
const rows = computed(() =>
  shares.value.map(share => {
    const reasons = shareReasons(share, requiredBytes.value)
    if (serverBusyPath.value === share.path && !reasons.includes('busy')) reasons.push('busy')
    if (failure.value?.sharePath === share.path && failure.value.key === 'rootfsClean.error.destinationFull') {
      if (!reasons.includes('space')) reasons.push('space')
    }
    return { share, reasons }
  }),
)
const selectedRow = computed(() => rows.value.find(row => row.share.path === selectedPath.value) ?? null)

/** The texts shown on the refused destination rows, including the last server refusal. */
function destinationReasons(row: { share: CleanupShare; reasons: ShareBlockReason[] }) {
  const texts = row.reasons.map(reason =>
    failure.value?.sharePath === row.share.path && failure.value.key === 'rootfsClean.error.destinationFull' && reason === 'space'
      ? t(failure.value.key)
      : rowReason(row.share, reason),
  )
  const refused = failure.value
  if (refused?.sharePath === row.share.path && reasonIsDestinationRefusal(refused.key) && refused.key !== 'rootfsClean.error.destinationFull') {
    texts.push(t(refused.key))
  }
  return texts
}

/** The shares the user can choose as the move destination, grouped and searchable above 20. */
const destinationOptions = computed(() => rows.value.map(row => ({
  value: row.share.path,
  label: row.share.name,
  group: shareLabel(row.share),
  meta: t('rootfsClean.destination.row', { volume: shareLabel(row.share), free: formatBytes(row.share.freeBytes) }),
  disabledReason: joinParts(destinationReasons(row), locale) || undefined,
})))

function chooseDestination(value: string[]) {
  if (planning.value) abandonPreview()
  selectedPath.value = value[0] ?? null
  failure.value = null
}
const selectedHasDestinationRefusal = computed(() =>
  !!failure.value && failure.value.sharePath === selectedPath.value && reasonIsDestinationRefusal(failure.value.key),
)
const allLackSpace = computed(() => rows.value.length > 0 && rows.value.every(row => row.reasons.includes('space')))
const moves = computed(() => plan.value?.moves ?? { bytes: 0, required_destination_bytes: 0, items: 0, by_category: [] })
const planItems = computed(() => plan.value?.items ?? [])
const smallMoveItems = computed(() => splitSmallMoveItems(planItems.value))
const tools = computed(() => plan.value?.tools ?? [])
const heldOpen = computed(() => plan.value?.held_open ?? [])
const kept = computed(() => plan.value?.kept ?? [])
const planIsEmpty = computed(() => plan.value?.plan_id == null && moves.value.items === 0 && tools.value.length === 0)
const folderLabel = computed(() => {
  const path = plan.value?.destination?.move_dir.path
  if (!path) return selectedRow.value?.share.name ?? ''
  const folder = path.slice(path.lastIndexOf('/') + 1)
  return `${selectedRow.value?.share.name ?? ''} › ${folder}`
})

let planningController = new AbortController()
onBeforeUnmount(() => planningController.abort())
watch(isOpen, value => {
  if (!value) { planningController.abort(); checked.value = false; deepChecked.value = false }
})

function pathValue(value: Path): string {
  return typeof value === 'string' ? value : value.path
}

function shareLabel(share: CleanupShare) {
  return formatVolume(share.mount)
}

function rowReason(share: CleanupShare, reason: ShareBlockReason) {
  return reason === 'space'
    ? t('rootfsClean.destination.tooSmall', { bytes: formatBytes(requiredBytes.value) })
    : t('rootfsClean.destination.busy', { volume: shareLabel(share) })
}

function itemCategory(category: string) {
  const known = ['update_leftover', 'log_archive', 'crash_dump', 'temp_debris', 'geoip_database', 'preference_image', 'samba_cache', 'tool_cache', 'backup_installer']
  return t(known.includes(category) ? `rootfsClean.category.${category}` : 'rootfsClean.category.unknown')
}

function toolLabel(tool: string) {
  const known = ['rotate_logs', 'delete_journal', 'logrotate', 'restart_syslog_ng', 'clean_downloaded_update', 'release_update_reserve', 'delete_log']
  return t(known.includes(tool) ? `rootfsClean.tool.${tool}` : 'rootfsClean.tool.unknown')
}

function keepReason(reason: string) {
  const known = ['recent_log_archive', 'small_junior_log', 'updated_since_boot', 'too_large', 'unrecognised_image', 'update_room_enough']
  return t(known.includes(reason) ? `rootfsClean.keep.${reason}` : 'rootfsClean.keep.unknown')
}

function setFailure(code?: string, sharePath?: string, holderCommand?: string) {
  const key = refusalMessageKey(code, holderCommand)
  const rootfsLockBusy = key === 'rootfsClean.error.rootfsBusy'
  failure.value = { key, sharePath: rootfsLockBusy ? undefined : sharePath }
  if (code === 'busy' && !rootfsLockBusy) serverBusyPath.value = sharePath ?? selectedPath.value
}

function updateData(nextReport: RootfsReportDocument, nextVolumes: { volumes: Volume[] }, nextJobs: Job[]) {
  if (nextReport.completion !== 'complete') {
    setFailure(nextReport.errors?.[0]?.code)
    return false
  }
  if (!Number.isFinite(nextReport.cleanable.required_destination_bytes)) {
    failure.value = { key: 'rootfsClean.state.loadFailed' }
    return false
  }

  report.value = nextReport
  volumeDocument.value = nextVolumes
  jobs.value = nextJobs
  if (nextReport.filesystem.level === 'ok') {
    stage.value = 'normal'
    selectedPath.value = null
    plan.value = null
    failure.value = null
    return true
  }

  stage.value = 'destination'
  const preferred = defaultDestination(shares.value, requiredBytes.value)
  if (!selectedPath.value || !rows.value.some(row => row.share.path === selectedPath.value)) {
    selectedPath.value = preferred?.path ?? null
  }
  return true
}

async function loadData(signal = planningController.signal) {
  const [nextReport, nextVolumes, nextJobs] = await Promise.all([
    runInBackground('rootfs.report', {}, { signal }),
    api('volumes.list', {}).then(document => {
      if (!signal.aborted) {
        volumeDocument.value = document
        selectedPath.value ??= defaultDestination(shares.value, requiredBytes.value)?.path ?? null
      }
      return document
    }),
    api('jobs.list', {}),
  ])
  if (signal.aborted) return false
  if (nextReport.completion === 'failed') {
    setFailure(nextReport.errors[0]?.code, undefined, nextReport.errors[0]?.holder?.command)
    return false
  }
  return updateData(nextReport, nextVolumes, nextJobs.jobs)
}

/** Open S4 and load the current report, destination shares and volume locks. */
async function open() {
  planningController.abort()
  planningController = new AbortController()
  const controller = planningController
  isOpen.value = true
  stage.value = 'destination'
  loading.value = true
  planning.value = false
  applying.value = false
  mode.value = 'standard'
  deepChecked.value = false
  report.value = null
  plan.value = null
  volumeDocument.value = null
  jobs.value = []
  selectedPath.value = null
  checked.value = false
  failure.value = null
  serverBusyPath.value = null
  try {
    await loadData(controller.signal)
  } catch (error) {
    if (controller.signal.aborted) return
    failure.value = error instanceof ApiError && error.code === 'busy'
      ? { key: refusalMessageKey('rootfs_busy') }
      : { key: 'rootfsClean.state.loadFailed' }
  } finally {
    if (planningController === controller) loading.value = false
  }
}

async function next() {
  const destination = selectedRow.value
  if (!destination || destination.reasons.length || planning.value) return
  planningController.abort()
  planningController = new AbortController()
  planning.value = true
  const controller = planningController
  const requestedMode = mode.value
  const requestedPath = selectedPath.value
  failure.value = null
  serverBusyPath.value = null
  try {
    if (!(await authorizeRootWork())) return
    const loaded = await loadData(controller.signal)
    if (controller.signal.aborted || requestedMode !== mode.value || requestedPath !== selectedPath.value) return
    if (!loaded || stage.value === 'normal') return

    const freshDestination = selectedRow.value
    if (!freshDestination) {
      selectedPath.value = null
      failure.value = null
      return
    }
    if (freshDestination.reasons.length || selectedHasDestinationRefusal.value) {
      const code = freshDestination.reasons.includes('busy') ? 'busy' : 'destination_full'
      setFailure(code, freshDestination.share.path)
      return
    }

    const result = await runInBackground('rootfs.plan', { to: freshDestination.share.path, mode: requestedMode }, { signal: controller.signal })
    if (!isOpen.value || controller.signal.aborted) return
    if (result.completion !== 'complete') {
      const firstError = result.errors[0]
      setFailure(firstError?.code ?? 'unknown', freshDestination.share.path, firstError?.holder?.command)
      return
    }
    if ((result.mode ?? 'standard') !== mode.value) {
      setFailure('plan_untrusted')
      return
    }
    plan.value = result
    checked.value = false
    deepChecked.value = false
    failure.value = null
    stage.value = 'review'
  } catch (error) {
    if (controller.signal.aborted) return
    setFailure(error instanceof ApiError && ['busy', 'backend_busy'].includes(error.code) ? 'rootfs_busy' : undefined, destination.share.path)
  } finally {
    if (planningController === controller) planning.value = false
  }
}

function back() {
  stage.value = 'destination'
  plan.value = null
  checked.value = false
  deepChecked.value = false
  failure.value = null
}

async function requestApply() {
  if (!checked.value || !plan.value?.plan_id || applying.value || (mode.value === 'deep' && !deepChecked.value)) return
  applying.value = true
  try {
    if (!(await authorizeRootWork())) return
    stage.value = 'apply'
  } finally {
    applying.value = false
  }
}

function reasonIsDestinationRefusal(key: string) {
  return key.startsWith('rootfsClean.error.destination')
}

defineExpose({ open })
</script>

<template>
  <Dialog v-model:open="isOpen" :aria-describedby="undefined" :title="t('rootfsClean.title')" :close-label="t('common.close')">
    <div class="rootfs-clean">
      <div v-if="stage !== 'normal'" class="rootfs-clean__step" aria-live="polite">
        <span>{{ t('rootfsClean.step', { count: stage === 'apply' ? 3 : stage === 'review' ? 2 : 1 }) }}</span>
        <span class="rootfs-clean__step-dots" aria-hidden="true">
          <i class="is-current" /><i :class="{ 'is-current': stage === 'review' || stage === 'apply' }" /><i :class="{ 'is-current': stage === 'apply' }" />
        </span>
      </div>

      <template v-if="stage === 'normal'">
        <Callout tone="warning">{{ t('rootfsClean.state.normalAgain') }}</Callout>
      </template>
      <template v-else-if="stage === 'destination'">
        <p v-if="loading || planning" role="status" class="rootfs-clean__quiet">{{ t('rootfsClean.state.checking') }}</p>
        <section data-testid="rootfs-mode" class="rootfs-clean__section">
          <h2>{{ t('rootfsClean.mode.title') }}</h2>
          <OptionPicker v-bind="{ ariaLabel: t('rootfsClean.mode.title') }" :model-value="[mode]" :options="modeOptions" @update:model-value="chooseMode" />
        </section>
        <section class="rootfs-clean__intro">
          <h2>{{ t('rootfsClean.destination.title') }}</h2>
          <p>{{ t('rootfsClean.destination.body') }}</p>
        </section>

        <Callout v-if="failure && !reasonIsDestinationRefusal(failure.key)" :dismissible="false" tone="warning" role="alert">
          {{ t(failure.key) }}
          <template v-if="failure.key === 'rootfsClean.state.loadFailed'" #actions>
            <Button variant="secondary" @click="open">{{ t('common.retry') }}</Button>
          </template>
        </Callout>
        <Callout v-if="!loading && !rows.length" :dismissible="false" tone="warning">{{ t('rootfsClean.destination.noShares') }}</Callout>
        <Callout v-else-if="allLackSpace" :dismissible="false" tone="warning">
          {{ t('rootfsClean.destination.noRoom', { bytes: formatBytes(requiredBytes) }) }}
        </Callout>

        <OptionPicker
          v-if="rows.length"
          data-testid="rootfs-destination"
          v-bind="{ ariaLabel: t('rootfsClean.destination.title') }"
          :model-value="selectedPath ? [selectedPath] : []"
          :options="destinationOptions"
          @update:model-value="chooseDestination"
        />

        <p v-if="selectedRow && scanningMounts.has(selectedRow.share.mount)" class="rootfs-clean__folder-note">{{ t('rootfsClean.destination.scanNote') }}</p>
        <p class="rootfs-clean__folder-note">{{ t('rootfsClean.destination.folderNote') }}</p>
      </template>
      <template v-else-if="stage === 'review' && plan">
        <p><strong>{{ t(`rootfsClean.mode.${mode}`) }}</strong> · {{ t('rootfsClean.mode.extra', { bytes: formatBytes(plan.additional_bytes ?? 0) }) }}</p>
        <Callout v-if="mode === 'deep'" tone="warning">{{ t('rootfsClean.mode.deepBody') }}</Callout>
        <Callout v-if="planIsEmpty" :dismissible="false" tone="warning">{{ t('rootfsClean.state.nothingToMove') }}</Callout>
        <template v-else>
          <div class="rootfs-clean__stats">
            <Surface variant="inset"><Statistic :label="t('rootfsClean.review.moves')" :value="t('rootfsClean.review.movesValue', { bytes: formatBytes(moves.bytes), count: formatCount(moves.items) })" /></Surface>
            <Surface variant="inset"><Statistic :label="t('rootfsClean.review.spaceAfter')" :value="`${formatBytes(plan.root?.available_bytes)} → ${formatBytes(plan.root?.available_after_bytes)}`" /></Surface>
            <Surface variant="inset" class="rootfs-clean__stat--destination">
              <Statistic :label="t('rootfsClean.review.destination')">
                <span class="rootfs-clean__destination-path" :title="plan.destination?.move_dir.path">{{ folderLabel }}</span>
              </Statistic>
            </Surface>
          </div>

          <p v-if="moves.items === 0 && tools.length" class="rootfs-clean__only-tools">{{ t('rootfsClean.review.onlyTools') }}</p>
          <section v-else class="rootfs-clean__section">
            <h2>{{ t('rootfsClean.review.items') }}</h2>
            <ReclaimGrid
              :fill="false"
              table-id="rootfs-review-categories"
              class="rootfs-clean__category-list"
              :columns="[
                { key: 'name', label: t('rootfsClean.review.items'), sortable: true },
                { key: 'items', label: t('recycle.col.items'), sortable: true, align: 'right' },
                { key: 'bytes', label: t('explore.table.size'), sortable: true, align: 'right' },
              ]"
              :rows="moves.by_category.map(entry => ({ ...entry, id: entry.category, name: itemCategory(entry.category) }))"
            >
              <template #cell-items="{ value }">{{ formatCount(value as number) }}</template>
              <template #cell-bytes="{ value }">{{ formatBytes(value as number) }}</template>
            </ReclaimGrid>
            <ul class="rootfs-clean__details rootfs-clean__impacts">
              <li v-for="entry in moves.by_category" :key="entry.category">
                <strong>{{ itemCategory(entry.category) }}:</strong> {{ t(`rootfsClean.impact.${entry.category}`) }}
              </li>
            </ul>
            <ExpandableItem v-if="planItems.length" :title="t('rootfsClean.review.fileDetails')">
              <template #detail>
                <ReclaimGrid
                  :fill="false"
                  table-id="rootfs-review-files"
                  :columns="[
                    { key: 'path', label: t('explore.table.name'), sortable: true },
                    { key: 'bytes', label: t('explore.table.size'), sortable: true, align: 'right' },
                  ]"
                  :rows="smallMoveItems.visible.map(item => ({ ...item, id: item.path }))"
                >
                  <template #cell-path="{ value }"><span :title="String(value)">{{ value }}</span></template>
                  <template #cell-bytes="{ value }">{{ formatBytes(value as number) }}</template>
                </ReclaimGrid>
                <p v-if="smallMoveItems.foldedCount">
                  {{ t('rootfsClean.review.smallFiles', { count: formatCount(smallMoveItems.foldedCount), bytes: formatBytes(smallMoveItems.foldedBytes) }) }}
                </p>
              </template>
            </ExpandableItem>
          </section>

          <section v-if="tools.length" class="rootfs-clean__tools">
            <strong>{{ t('rootfsClean.review.tools') }}:</strong>
            &nbsp;
            <ul class="rootfs-clean__details">
              <li v-for="tool in tools" :key="tool.tool">
                <span>{{ toolLabel(tool.tool) }} — {{ joinSentences([t(`rootfsClean.toolImpact.${tool.tool}`), ...(tool.estimated_bytes == null ? [t('rootfsClean.mode.toolEstimate')] : [])], locale) }}</span>
                <span v-if="tool.estimated_bytes != null" class="rootfs-clean__tool-size">{{ formatBytes(tool.estimated_bytes) }}</span>
              </li>
            </ul>
          </section>

          <div v-if="heldOpen.length" class="rootfs-clean__quiet">
            {{ t(heldOpen.length === 1 ? 'rootfsClean.review.heldOpen.one' : 'rootfsClean.review.heldOpen.other', { count: formatCount(heldOpen.length) }) }}
            <ExpandableItem :title="t('rootfsClean.review.fileDetails')">
              <template #detail>
                <ul class="rootfs-clean__details"><li v-for="item in heldOpen" :key="item.path">{{ t('rootfsClean.review.heldFile', { path: item.path, holder: item.holder, bytes: formatBytes(item.bytes) }) }}</li></ul>
              </template>
            </ExpandableItem>
          </div>

          <section v-if="kept.length" class="rootfs-clean__kept">
            <h2>{{ t('rootfsClean.review.kept') }}</h2>
            <ul><li v-for="item in kept" :key="item.path">{{ t('rootfsClean.review.keptFile', { path: item.path, reason: keepReason(item.reason), bytes: formatBytes(item.bytes) }) }}</li></ul>
          </section>

          <Callout :dismissible="false" tone="warning" :title="t('rootfsClean.review.noUndoTitle')">{{ t('rootfsClean.review.noUndoBody', { folder: folderLabel }) }}</Callout>
          <Checkbox v-model="checked" :label="t('rootfsClean.review.check')" />
          <Checkbox v-if="mode === 'deep'" v-model="deepChecked" :label="t('rootfsClean.mode.confirm')" />
        </template>
      </template>

      <RootfsApplyStep
        v-else-if="stage === 'apply' && plan?.plan_id"
        :open="isOpen"
        :plan-id="plan.plan_id"
        :mode="mode"
        @close="isOpen = false"
        @complete="emit('complete', $event)"
        @check-again="back(); emit('refresh')"
      />

      <Callout v-if="stage === 'review' && failure" :dismissible="false" tone="warning" role="alert">{{ t(failure.key) }}</Callout>
    </div>

    <template #footer>
      <div class="rootfs-clean__footer">
        <template v-if="stage === 'destination'">
          <Button variant="secondary" @click="isOpen = false">{{ t('common.cancel') }}</Button>
          <Button variant="primary" :disabled="!selectedRow || !!selectedRow.reasons.length || selectedHasDestinationRefusal || planning || loading || !report" :loading="planning" @click="next">
            {{ planning ? t('rootfsClean.state.checking') : t('rootfsClean.action.next') }}
          </Button>
        </template>
        <template v-else-if="stage === 'review' && plan">
          <Button variant="secondary" :disabled="applying" @click="back">{{ t('rootfsClean.action.back') }}</Button>
          <Button v-if="!planIsEmpty" variant="danger" :disabled="!checked || !plan.plan_id || applying || (mode === 'deep' && !deepChecked)" :loading="applying" @click="requestApply">
            {{ t('rootfsClean.action.yes') }}
          </Button>
        </template>
        <template v-else>
          <Button variant="primary" @click="isOpen = false">{{ t('common.close') }}</Button>
        </template>
      </div>
    </template>
  </Dialog>
</template>

<style scoped>
.rootfs-clean { display: grid; gap: 9px; color: var(--ui-text); }
.rootfs-clean__step { display: flex; align-items: center; justify-content: flex-end; gap: 12px; color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); }
.rootfs-clean__step-dots { display: flex; gap: 5px; }
.rootfs-clean__step-dots i { width: 22px; height: 4px; border-radius: var(--ui-radius-pill); background: var(--ui-border); }
.rootfs-clean__step-dots i.is-current { background: var(--ui-primary); }
.rootfs-clean__intro h2, .rootfs-clean__section h2, .rootfs-clean__kept h2 { margin: 0 0 8px; font-size: var(--ui-font-size-lg); font-weight: 650; }
.rootfs-clean__intro p { margin: 0; max-width: 720px; color: var(--ui-text-secondary); line-height: 1.5; }
.rootfs-clean__folder-note, .rootfs-clean__quiet { margin: 0; color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); }
.rootfs-clean__stats { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
.rootfs-clean__stat--destination { grid-column: 1 / -1; }
.rootfs-clean__destination-path { font-size: var(--ui-font-size); font-weight: 400; overflow-wrap: anywhere; }
.rootfs-clean__category-list { display: grid; }
.rootfs-clean__category > span:last-child { flex: none; color: var(--ui-text-muted); font-variant-numeric: tabular-nums; }
.rootfs-clean__tool-size { display: block; color: var(--ui-text-muted); font-variant-numeric: tabular-nums; }
.rootfs-clean__tools { color: var(--ui-text-secondary); font-size: var(--ui-font-size); line-height: 1.4; }
.rootfs-clean__kept ul, .rootfs-clean__details { margin: 6px 0 0; padding-left: 19px; }
.rootfs-clean__kept li, .rootfs-clean__details li { margin: 3px 0; line-height: 1.4; overflow-wrap: anywhere; }
.rootfs-clean__kept { padding-top: 2px; }
.rootfs-clean__kept ul { color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.rootfs-clean__details { color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.rootfs-clean__only-tools { margin: 0; font-weight: 600; }
.rootfs-clean__footer { display: flex; justify-content: flex-end; gap: 8px; width: 100%; }
@media (max-width: 720px) {
  .rootfs-clean__stats { grid-template-columns: 1fr 1fr; }
  .rootfs-clean__stat--destination { grid-column: 1 / -1; }
}
</style>
