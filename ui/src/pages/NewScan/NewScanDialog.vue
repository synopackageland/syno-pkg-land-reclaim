<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Button, Callout, Checkbox, Dialog, LoadingMask, RadioGroup, Space } from '@synopackageland/ui'
import type { RadioOption } from '@synopackageland/ui'
import { api, ApiError } from '@/api'
import type { HistoryDocument, Job, Volume } from '@/types'
import { code, formatBytes, formatDateTime, formatDuration, formatVolume, joinParts, locale, t } from '@/i18n'
import { apiErrorText } from '@/components/states/apiErrorText'
import UnsupportedVolumesNote from '@/components/states/UnsupportedVolumesNote.vue'
import { loadVolumes, volumeList, volumes } from '@/stores/volumes'
import { jobs, runningJobs, pollNow } from '@/stores/jobs'
import { checkRootfs, navigate, openJob } from '@/stores/nav'
import { authorizeRootWork } from '@/stores/authorization'
import type { ScanMode } from '@/api/methods'
import { isVisiblePausedJob, modeForProfile } from '@/components/scanProgress'
import OptionPicker from '@/components/OptionPicker.vue'
import VolumePicker from '@/components/VolumePicker.vue'
import { VOLUME_TABS_MAX } from '@/components/optionList'
import { SCAN_TARGET_LIMIT, exceedsTargetLimit } from '@/components/scanTarget'
import { useNow } from '@/components/useNow'
import { volumeUsage } from '@/pages/Overview/overview'
import { useOverviewData } from '@/pages/Overview/useOverviewData'
import { systemCheckOffered } from '@/pages/Overview/systemPartition'

const open = defineModel<boolean>('open', { required: true })
/** The volume list's System partition choice: not a scan, the System partition view's Check again. */
const SYSTEM_PARTITION = '@system-partition'
const selectedMount = ref('')
const scope = ref<'volume' | 'shares'>('volume')
const mode = ref<ScanMode>('gentle')
// A whole-volume scan includes snapshots unless the administrator unticks it.
const includeSnapshots = ref(true)
const selectedShares = ref<string[]>([])
const history = ref<HistoryDocument | null>(null)
/** The volumes and history estimates the setup shows are still loading; a mask covers the setup. */
const preparing = ref(false)
const starting = ref(false)
const actionError = ref<'busy' | 'failed' | null>(null)
/** Why the last action failed, already localized. */
const actionErrorText = ref('')
const resumeOpen = ref(false)
const cancelPausedOpen = ref(false)
const resumeMode = ref<ScanMode>('gentle')
const now = useNow()
const overviews = useOverviewData()

const volumesList = computed(() => volumeList.value)
const systemPartition = computed(() => {
  const partition = volumes.document?.system_partition
  return systemCheckOffered(partition) ? partition! : null
})
const systemChosen = computed(() => selectedMount.value === SYSTEM_PARTITION && !!systemPartition.value)
const selectedVolume = computed(() => volumesList.value.find(volume => volume.mount.path === selectedMount.value) ?? null)
const refusedJobId = ref<string | null>(null)
const busyJob = computed(() => runningJobs.value.find(job => job.kind === 'scan') ?? null)
const visiblePausedJobs = computed(() => jobs.list.filter(job => job.kind === 'scan' && isVisiblePausedJob(job, now.value)))
const selectedPausedJob = computed(() => selectedVolume.value
  ? visiblePausedJobs.value.find(job => targetsVolume(job, selectedVolume.value!.mount.path)) ?? null
  : null)
const pausedJob = computed(() => selectedPausedJob.value ?? visiblePausedJobs.value[0] ?? null)
const pausedTooLong = computed(() => Boolean(pausedJob.value
  && now.value - Date.parse(pausedJob.value.updated) > 5 * 60_000))
const canChooseScan = computed(() => Boolean(
  selectedVolume.value?.metadata_access && !busyJob.value && !refusedJobId.value
  && (scope.value === 'volume' || (selectedShares.value.length > 0 && !targetLimitExceeded.value)),
))
const canStart = computed(() => canChooseScan.value && !selectedPausedJob.value && !starting.value && !preparing.value)
const targetLimitExceeded = computed(() => exceedsTargetLimit(selectedShares.value.length))
/** The volume has more shares than one scan takes; the list caps and hints at the whole volume. */
const tooManyShares = computed(() => (selectedVolume.value?.shares.length ?? 0) > SCAN_TARGET_LIMIT)
const volumeChoices = computed(() => [
  ...volumesList.value.map(volume => ({
    value: volume.mount.path,
    label: formatVolume(volume.mount.path),
    meta: usageText(volume),
    disabledReason: volume.metadata_access ? undefined : t('newScan.volume.noMetadata'),
  })),
  ...(systemPartition.value ? [{ value: SYSTEM_PARTITION, label: t('rootfs.name'), meta: t('newScan.system.free', { free: formatBytes(systemPartition.value.available_bytes) }) }] : []),
])
const volumeRadios = computed<RadioOption[]>(() => [
  ...volumesList.value.map(volume => {
    const duration = lastScanDuration(volume)
    return {
      value: volume.mount.path,
      label: formatVolume(volume.mount.path),
      description: joinParts([
        t('newScan.volume.shareCount', { count: volume.shares.length }),
        usageText(volume),
        duration != null ? t('newScan.volume.lastScan', { duration: formatDuration(duration) }) : t('newScan.volume.noScan'),
        ...(volume.metadata_access ? [] : [t('newScan.volume.noMetadata')]),
      ], locale),
      disabled: !volume.metadata_access,
    }
  }),
  ...(systemPartition.value
    ? [{
        value: SYSTEM_PARTITION,
        label: t('rootfs.name'),
        description: joinParts([t('newScan.system.free', { free: formatBytes(systemPartition.value.available_bytes) }), t('newScan.system.help')], locale),
      }]
    : []),
])
const sharesEmpty = computed(() => !selectedVolume.value?.shares.length)
const scopeRadios = computed<RadioOption[]>(() => {
  const ext4 = selectedVolume.value?.filesystem === 'ext4'
  return [
    { value: 'volume', label: t('newScan.scope.volume'), description: t(ext4 ? 'newScan.scope.ext4VolumeHelp' : 'newScan.scope.volumeHelp') },
    { value: 'shares', label: t('newScan.scope.shares'), description: t(ext4 ? 'newScan.scope.ext4SharesHelp' : 'newScan.scope.sharesHelp'), disabled: sharesEmpty.value },
  ]
})
const modeRadios = computed<RadioOption[]>(() => [
  { value: 'fast', label: t('newScan.mode.fast'), description: t('newScan.mode.fastHelp') },
  { value: 'gentle', label: t('newScan.mode.gentle'), description: t('newScan.mode.gentleHelp') },
])
const resumeModeRadios = computed<RadioOption[]>(() => [
  { value: 'fast', label: t('scan.mode.fast'), description: t('scan.mode.fastHelp') },
  { value: 'gentle', label: t('scan.mode.gentle'), description: t('scan.mode.gentleHelp') },
])
const shareChoices = computed(() => (selectedVolume.value?.shares ?? []).map(share => ({
  value: share.path.path,
  label: share.name.path,
  meta: shareSize(selectedVolume.value!, share),
})))

function targetsVolume(job: Job, mount: string) {
  return job.filesystems.some(filesystem => filesystem.mount.path === mount)
    || job.targets.some(target => target.path === mount || target.path.startsWith(`${mount}/`))
}

function startVolumeSelection() {
  const next = volumesList.value.find(volume => volume.mount.path === volumes.selected && volume.metadata_access)
    ?? volumesList.value.find(volume => volume.metadata_access)
    ?? volumesList.value[0]
  selectedMount.value = next?.mount.path ?? ''
  selectedShares.value = []
}

watch(selectedMount, () => {
  selectedShares.value = []
  scope.value = 'volume'
  // Only the shown volume's report is needed for its card (review §1 P0-4).
  if (open.value && selectedVolume.value) void overviews.load(selectedMount.value)
})

watch(open, async isOpen => {
  if (!isOpen) return
  preparing.value = true
  pollNow()
  if (!volumes.document) await loadVolumes()
  startVolumeSelection()
  if (selectedMount.value) void overviews.load(selectedMount.value)
  scope.value = 'volume'
  mode.value = 'gentle'
  includeSnapshots.value = true
  history.value = null
  actionError.value = null
  refusedJobId.value = null
  try {
    history.value = await api('history.list', {})
  } catch {
    // A missing optional estimate must not prevent a scan.
  } finally {
    preparing.value = false
  }
}, { immediate: true })

watch(scope, () => {
  includeSnapshots.value = true
})

function usageText(volume: Volume) {
  const { used, total } = volumeUsage(volume, overviews.answers[volume.mount.path]?.overview?.results[0])
  return `${formatBytes(used)} / ${formatBytes(total)}`
}

function lastScanDuration(volume: Volume) {
  const previous = history.value?.scans.find(scan => scan.volume
    && scan.targets.some(target => target.path === volume.mount.path)
    && (scan.outcome === 'completed' || scan.outcome === 'partial'))
  if (previous) return (previous.full_read_ms ?? previous.total_elapsed_ms ?? previous.elapsed_ms) / 1000
  const hint = volumes.document?.scan_hints?.find(item => item.path === volume.mount.path)?.last_scan
  return hint ? hint.elapsed_ms / 1000 : null
}

function shareSize(volume: Volume, share: Volume['shares'][number]) {
  if (share.size_bytes != null) return formatBytes(share.size_bytes)
  return volume.reports === 0 ? t('newScan.notScanned') : t('common.unknown')
}

function showProgress() {
  open.value = false
  const id = refusedJobId.value ?? busyJob.value?.job_id ?? pausedJob.value?.job_id
  if (id) openJob({ job_id: id, kind: 'scan' })
  else navigate('scanning')
}

function openResume() {
  const job = pausedJob.value
  if (!job || !job.compatible || busyJob.value) return
  resumeMode.value = modeForProfile(job.profile)
  actionError.value = null
  actionErrorText.value = ''
  resumeOpen.value = true
}

async function resumePaused(fresh = false) {
  const job = pausedJob.value
  if (!job || (!fresh && !job.compatible) || starting.value) return
  starting.value = true
  actionError.value = null
  try {
    if (!(await authorizeRootWork())) return
    const response = await api('job.resume', fresh ? { job_id: job.job_id, fresh: true } : { job_id: job.job_id, mode: resumeMode.value })
    if (response.outcome === 'scan_in_progress') {
      refusedJobId.value = response.running_job_id ?? null
      actionError.value = 'busy'
      actionErrorText.value = code('api', 'scan_in_progress')
      return
    }
    openJob({ job_id: response.job_id ?? job.job_id, kind: 'scan' })
    volumes.selected = job.filesystems[0]?.mount.path ?? job.targets[0]?.path ?? null
    resumeOpen.value = false
    open.value = false
    navigate('scanning')
    pollNow()
  } catch (error) {
    actionError.value = error instanceof ApiError && error.code === 'scan_in_progress' ? 'busy' : 'failed'
    if (error instanceof ApiError && typeof error.detail?.running_job_id === 'string') refusedJobId.value = error.detail.running_job_id
    actionErrorText.value = apiErrorText(error)
  } finally {
    starting.value = false
  }
}

async function startScan(replacePaused = false) {
  const volume = selectedVolume.value
  const paused = selectedPausedJob.value
  if (!volume || starting.value || !canChooseScan.value || (paused && !replacePaused) || (!paused && replacePaused)) return
  const targets = scope.value === 'volume'
    ? [volume.mount.path]
    : volume.shares.filter(share => selectedShares.value.includes(share.path.path)).map(share => share.path.path)
  starting.value = true
  actionError.value = null
  try {
    if (!(await authorizeRootWork())) return
    if (paused) {
      const response = await api('job.cancel', { job_id: paused.job_id })
      if (response.outcome !== 'cancelled') {
        actionError.value = ['busy', 'too_late'].includes(response.outcome) ? 'busy' : 'failed'
        actionErrorText.value = code('error', response.outcome)
        pollNow()
        return
      }
    }
    volumes.selected = volume.mount.path
    const response = await api('scan.start', { targets, mode: mode.value, snapshots: scope.value === 'volume' && includeSnapshots.value && selectedVolume.value?.capabilities.snapshots === true })
    if (response.outcome === 'scan_in_progress') {
      refusedJobId.value = response.running_job_id ?? null
      actionError.value = 'busy'
      pollNow()
      return
    }
    if (response.job_id) openJob({ job_id: response.job_id, kind: 'scan' })
    open.value = false
    navigate('scanning')
    pollNow()
  } catch (error) {
    actionError.value = error instanceof ApiError && error.code === 'scan_in_progress' ? 'busy' : 'failed'
    if (error instanceof ApiError && error.code === 'scan_in_progress') {
      refusedJobId.value = typeof error.detail?.running_job_id === 'string' ? error.detail.running_job_id : null
      pollNow()
    }
    actionErrorText.value = apiErrorText(error)
  } finally {
    starting.value = false
  }
}

/** Hand the check to the Overview, which shows its progress and result. */
function checkSystemPartition() {
  open.value = false
  checkRootfs()
}

async function cancelPaused() {
  const job = pausedJob.value
  if (!job || starting.value) return
  starting.value = true
  actionError.value = null
  try {
    if (!(await authorizeRootWork())) return
    const response = await api('job.cancel', { job_id: job.job_id })
    if (response.outcome !== 'cancelled') {
      actionError.value = ['busy', 'too_late'].includes(response.outcome) ? 'busy' : 'failed'
      actionErrorText.value = code('error', response.outcome)
    } else {
      cancelPausedOpen.value = false
    }
    pollNow()
  } catch (error) {
    actionError.value = 'failed'
    actionErrorText.value = apiErrorText(error)
  } finally {
    starting.value = false
  }
}
</script>

<template>
  <Dialog
    v-model:open="open" :title="t('newScan.title')" :description="t('newScan.description')"
    :close-label="t('common.close')"
  >
    <LoadingMask :loading="preparing" :delay="200" :text="t('common.loading')">
      <div class="scan-setup">
        <Callout v-if="!systemChosen && (busyJob || actionError === 'busy')" :dismissible="false" tone="warning" :title="t('newScan.busy.title')">
          <span>{{ t('newScan.busy.body') }}</span>
          <template #actions>
            <Button size="sm" @click="showProgress">{{ t('newScan.busy.view') }}</Button>
          </template>
        </Callout>
        <section class="setup-section" :aria-labelledby="'volume-heading'">
          <h2 id="volume-heading">{{ t('newScan.volume.title') }}</h2>
          <RadioGroup
            v-if="volumesList.length <= VOLUME_TABS_MAX" v-model="selectedMount" :aria-label="t('newScan.volume.title')"
            :options="volumeRadios"
          />
          <VolumePicker
            v-else v-bind="{ ariaLabel: t('newScan.volume.title') }" :model-value="selectedMount" :volumes="volumeChoices"
            @update:model-value="selectedMount = $event"
          />
          <UnsupportedVolumesNote />
        </section>

        <section v-if="!systemChosen" class="setup-section">
          <h2>{{ t('newScan.scope.title') }}</h2>
          <RadioGroup v-model="scope" :aria-label="t('newScan.scope.title')" :options="scopeRadios" />
          <OptionPicker
            v-if="scope === 'shares' && selectedVolume"
            v-model="selectedShares"
            v-bind="{ ariaLabel: t('newScan.shares.title') }"
            multiple
            :options="shareChoices"
            :max-selected="SCAN_TARGET_LIMIT"
            :empty-text="t('newScan.shares.empty')"
          />
          <Callout
            v-if="targetLimitExceeded || tooManyShares"
            :tone="targetLimitExceeded ? 'warning' : 'info'"
            :title="t('newScan.shares.limitTitle')"
          >
            {{ t('newScan.shares.limitBody', { max: SCAN_TARGET_LIMIT }) }}
          </Callout>
        </section>

        <section v-if="!systemChosen" class="setup-section">
          <h2>{{ t('newScan.mode.title') }}</h2>
          <RadioGroup v-model="mode" :aria-label="t('newScan.mode.title')" :options="modeRadios" />
          <Checkbox
            v-if="scope === 'volume' && selectedVolume?.capabilities.snapshots" v-model="includeSnapshots" :label="t('newScan.snapshots.label')"
            :description="t('newScan.snapshots.help')" class="snapshot-option"
          />
        </section>

        <Callout v-if="pausedJob && !systemChosen" :dismissible="false" tone="warning" :title="t('newScan.paused.title', { volume: pausedJob.filesystems[0] ? formatVolume(pausedJob.filesystems[0].mount.path) : pausedJob.targets[0]?.path ?? t('common.unknown') })">
          <p v-if="pausedJob.state === 'interrupted'">{{ t('scan.interrupted', { time: formatDateTime(pausedJob.updated) }) }}</p>
          <p v-else>{{ t('scan.paused.body', { time: formatDateTime(pausedJob.updated), count: pausedJob.units }) }}</p>
          <p v-if="!pausedJob.compatible" class="paused-hint">{{ t('scan.upgrade.rescan') }}</p>
          <p v-if="pausedTooLong" class="paused-hint">{{ t('scan.paused.restartHint') }}</p>
          <p v-if="selectedPausedJob" class="paused-hint">{{ t('newScan.paused.startOverHelp') }}</p>
          <template #actions>
            <Space wrap :spacing="8" align-items="stretch" class="paused-actions">
              <Button v-if="pausedJob.compatible" size="sm" variant="primary" :disabled="Boolean(busyJob) || starting" @click="openResume">{{ t('scan.resume') }}</Button>
              <Button v-else size="sm" variant="primary" :disabled="Boolean(busyJob) || starting" @click="resumePaused(true)">{{ t('scan.idle.action') }}</Button>
              <Button v-if="selectedPausedJob" size="sm" variant="danger" :loading="starting" :disabled="!canChooseScan || starting" @click="startScan(true)">{{ t('newScan.paused.startOver') }}</Button>
              <Button size="sm" variant="ghost" :disabled="starting" @click="cancelPausedOpen = true">{{ t('scan.cancel') }}</Button>
            </Space>
            <Button size="sm" variant="ghost" @click="showProgress">{{ t('newScan.busy.view') }}</Button>
          </template>
        </Callout>
        <Callout v-if="selectedVolume && selectedVolume.reports > 0" :title="t('newScan.previous.title')">
          {{ t('newScan.previous.body') }}
          <p class="previous-hint">{{ t(selectedVolume?.filesystem === 'ext4' ? 'newScan.rescan.ext4Hint' : 'newScan.rescan.cacheHint') }}</p>
        </Callout>
        <Callout v-if="actionError === 'failed'" tone="danger" :title="t('newScan.error.title')">
          {{ t('newScan.error.body', { code: actionErrorText }) }}
        </Callout>
      </div>
    </LoadingMask>

    <template #footer>
      <div class="dialog-footer">
        <span>{{ t(systemChosen ? 'newScan.system.background' : 'newScan.background') }}</span>
        <div class="dialog-footer-actions">
          <Button :disabled="starting" @click="open = false">{{ t('common.cancel') }}</Button>
          <Button v-if="systemChosen" variant="primary" :disabled="preparing" @click="checkSystemPartition">{{ t('newScan.system.start') }}</Button>
          <Button v-else variant="primary" :loading="starting" :disabled="!canStart" @click="startScan()">
            {{ scope === 'volume' ? t('newScan.startVolume', { volume: selectedVolume ? formatVolume(selectedVolume.mount.path) : '' }) : t('newScan.startShares', { count: selectedShares.length }) }}
          </Button>
        </div>
      </div>
    </template>
  </Dialog>

  <Dialog v-model:open="resumeOpen" :title="t('scan.resume.title')" :description="t('scan.resume.body')" :close-label="t('common.close')">
    <Callout v-if="pausedTooLong" tone="warning" :title="t('scan.paused.restartHint')" />
    <Callout v-if="actionError" tone="danger" :title="t('scan.actionFailedTitle')">
      <p>{{ actionErrorText }}</p>
      <Button :disabled="starting" @click="resumePaused(true)">{{ t('scan.idle.action') }}</Button>
    </Callout>
    <RadioGroup v-model="resumeMode" :aria-label="t('scan.stats.mode')" :options="resumeModeRadios" />
    <template #footer>
      <Button :disabled="starting" @click="resumeOpen = false">{{ t('common.cancel') }}</Button>
      <Button variant="primary" :loading="starting" @click="resumePaused(false)">{{ t('scan.resume') }}</Button>
    </template>
  </Dialog>

  <Dialog v-model:open="cancelPausedOpen" :title="t('newScan.paused.cancelTitle')" :description="t('newScan.paused.cancelBody')" :close-label="t('common.close')">
    <template #footer>
      <Button :disabled="starting" @click="cancelPausedOpen = false">{{ t('newScan.paused.keep') }}</Button>
      <Button variant="danger" :loading="starting" @click="cancelPaused">{{ t('newScan.paused.cancelConfirm') }}</Button>
    </template>
  </Dialog>
</template>

<style scoped>
.scan-setup { display: grid; gap: 22px; padding: 2px 3px; }
.setup-section { display: grid; gap: 10px; }
h2 { color: var(--ui-text); font-size: var(--ui-font-size-lg); font-weight: 700; }
.snapshot-option { margin-top: 5px; }
.paused-hint { margin-top: 6px; }
.previous-hint { margin-top: 6px; }

.dialog-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); }
.dialog-footer-actions { display: flex; align-items: center; gap: 8px; }
@media (max-width: 700px) { .dialog-footer { align-items: stretch; flex-direction: column; } }
</style>
