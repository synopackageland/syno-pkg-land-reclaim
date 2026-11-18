<script setup lang="ts">
// The one progress view for a volume scan and for a System partition scan
// (rootfs-report). A System partition scan reports no fraction, phases or
// counts, so it shows an open ring with its elapsed time, its status and
// phase, and Cancel; it cannot pause or change mode, and when it ends its
// result opens on Overview's System partition card.
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { Button, LoadingArea, Callout, Dialog, EmptyState, RadioGroup, Statistic, Surface, useToast, Space } from '@synopackageland/ui'
import { api } from '@/api'
import type { Job, ScanDocument } from '@/types'
import type { ScanMode } from '@/api/methods'
import { useJobActions } from '@/components/jobActions'
import ProgressRing from '@/components/ProgressRing.vue'
import { densityForRate, scanRate, type FieldDensity } from '@/components/scanFieldModel'
import { useNow } from '@/components/useNow'
import PhaseList from '@/components/PhaseList.vue'
import StateView from '@/components/states/StateView.vue'
import CancelButton from '@/components/CancelButton.vue'
import { currentLocation, isVisiblePausedJob, liveElapsed, modeForProfile, progressRatio } from '@/components/scanProgress'
import { scopeLabel } from '@/components/scanTarget'
import { isActiveJob } from '@/stores/jobStates'
import { code, formatBytes, formatCount, formatDateTime, formatDuration, formatPercent, formatRelative, t } from '@/i18n'
import { apiErrorText } from '@/components/states/apiErrorText'
import { jobs, pollNow, runningJobs, shownState } from '@/stores/jobs'
import { volumes } from '@/stores/volumes'
import { checkRootfs, nav, navigate, openNewScan, openRootfsView } from '@/stores/nav'

/** The ring title per job state; any other state reads as running. */
const STATUS_KEYS: Partial<Record<Job['state'], string>> = {
  paused: 'paused',
  interrupted: 'interrupted',
  pausing: 'pausing',
  cancelling: 'cancelling',
  queued: 'queued',
}

const PAUSE_REASONS = ['paused', 'runtime_limit', 'resource_limit', 'disk_limit']

const toast = useToast()
const trackedJobId = ref<string | null>(nav.followedJobId)
watch(() => nav.followedJobId, id => { if (id) trackedJobId.value = id })
const result = ref<ScanDocument | null>(null)
const resultError = ref('')
const resultLoading = ref(false)
const actions = useJobActions()
const now = useNow(1000)
const actionBusy = computed(() => actions.pending.value !== null)
const { notice: actionNotice, failure: actionFailure } = actions
const cancelOpen = ref(false)
const pauseOpen = ref(false)
const resumeOpen = ref(false)
const resumeMode = ref<ScanMode>('gentle')
const resumeModeRadios = computed(() => [
  { value: 'fast', label: t('scan.mode.fast'), description: t('scan.mode.fastHelp') },
  { value: 'gentle', label: t('scan.mode.gentle'), description: t('scan.mode.gentleHelp') },
])
const handledResults = new Set<string>()

const trackedJob = computed(() => jobs.list.find(job => job.kind === 'scan' && job.job_id === trackedJobId.value) ?? null)
/** The followed System partition scan; it takes the page while it is followed. */
const reportJob = computed(() => jobs.list.find(job => job.kind === 'rootfs-report' && job.job_id === trackedJobId.value) ?? null)
const reportState = computed(() => reportJob.value ? shownState(reportJob.value) : null)
const reportActive = computed(() => reportState.value === 'queued' || reportState.value === 'running')
const reportDone = computed(() => reportState.value === 'completed' || reportState.value === 'partial')
const reportPhase = computed(() => reportState.value === 'queued' ? t('systemScan.waiting') : reportState.value === 'running' ? t('systemScan.reading') : code('state', reportState.value ?? 'unknown'))
const reportTitle = computed(() => t(`scan.status.${STATUS_KEYS[reportState.value ?? 'running'] ?? 'running'}`, { target: t('rootfs.name') }))
// Between polls a running walk's elapsed time keeps counting.
const reportElapsed = computed(() => {
  const job = reportJob.value
  if (job?.elapsed_s == null) return undefined
  const since = job.state === 'running' && jobs.polled ? Math.max(0, Math.floor((now.value - jobs.polled.receivedAt) / 1000)) : 0
  return job.elapsed_s + since
})
const visiblePausedJob = computed(() => jobs.list.find(job => job.kind === 'scan' && isVisiblePausedJob(job, now.value)) ?? null)
const currentJob = computed(() => {
  const tracked = trackedJob.value
  if (tracked && (isActiveJob(tracked) || isVisiblePausedJob(tracked, now.value) || ['completed', 'partial', 'failed', 'cancelled'].includes(tracked.state))) return tracked
  return runningJobs.value.find(job => job.kind === 'scan') ?? visiblePausedJob.value
})
// scan.start answers before jobs.list lists the job; until it does (or long
// after it should have), the page says the scan is starting, not that none runs.
const STARTING_WAIT_MS = 30_000
const followedSince = ref(Date.now())
const trackedSeen = ref(false)
watch(trackedJobId, () => {
  followedSince.value = Date.now()
  trackedSeen.value = false
})
watch(() => trackedJob.value ?? reportJob.value, job => { if (job) trackedSeen.value = true }, { immediate: true })
const startingScan = computed(() => !currentJob.value && !reportJob.value && trackedJobId.value != null && !trackedSeen.value
  && now.value - followedSince.value < STARTING_WAIT_MS)
const missingFinalResult = computed(() => currentJob.value != null && ['interrupted', 'failed'].includes(currentJob.value.state) && !result.value)
const progress = computed(() => currentJob.value?.progress)
const elapsed = computed(() => currentJob.value && liveElapsed(currentJob.value, jobs.polled, now.value))
const state = computed(() => (currentJob.value ? shownState(currentJob.value) : null))
const statusKey = computed(() => `scan.status.${STATUS_KEYS[state.value ?? 'running'] ?? 'running'}`)
const publishing = computed(() => currentJob.value?.phase === 'publish' || progress.value?.phase === 'publish')
const ratio = computed(() => publishing.value ? null : progressRatio(progress.value))
// Text longer than this no longer fits the ring's inner circle at the normal size.
const RING_LONG_TEXT = 7
const ringPercent = computed(() => ratio.value == null ? '' : formatPercent(ratio.value))
const ringCount = computed(() => formatCount(progress.value?.entries))
// The scan field glows brighter as the scan reads faster.
const fieldDensity = ref<FieldDensity>(2)
let lastSample: { entries: number; at: number } | null = null
watch(() => progress.value, value => {
  if (value?.entries == null) return
  const sample = { entries: value.entries, at: value.updated ? Date.parse(value.updated) : Date.now() }
  const rate = scanRate(lastSample, sample)
  if (rate != null) fieldDensity.value = densityForRate(rate)
  // Keep the older sample until the next one is far enough apart.
  if (rate != null || !lastSample || sample.entries < lastSample.entries) lastSample = sample
})
const progressIsStale = computed(() => !publishing.value && currentJob.value?.state === 'running'
  && progress.value?.updated != null
  && now.value - Date.parse(progress.value.updated) > 30_000)
const etaSeconds = computed(() => {
  const p = progress.value
  if (currentJob.value?.state !== 'running' || !p || p.basis == null || p.phase === 'snapshots') return null
  return p.basis.elapsed_s - p.elapsed_s
})
const etaLabel = computed(() => {
  if (currentJob.value?.state !== 'running') return ''
  if (publishing.value) return t('scan.progress.publishing')
  if (progressIsStale.value) return t('scan.progress.stalled')
  if (etaSeconds.value == null) return t('scan.progress.items', { entries: formatCount(progress.value?.entries) })
  if (etaSeconds.value <= 0) return t('scan.progress.overdue')
  return t('scan.progress.remaining', { duration: formatDuration(etaSeconds.value) })
})
const selectedVolume = computed(() => {
  const path = currentJob.value?.filesystems[0]?.mount.path
    ?? (currentJob.value?.scope === 'volume' ? currentJob.value.targets[0]?.path : undefined)
  return volumes.document?.volumes.find(volume => volume.mount.path === path) ?? null
})
const scanTarget = computed(() => (currentJob.value ? scopeLabel(currentJob.value.targets, currentJob.value.scope === 'volume') : ''))
/** The item being read now, named through its shared folder when that is known. */
const locationShares = computed(() => {
  const shares = (selectedVolume.value?.shares ?? []).map(share => ({ path: share.path, name: share.name }))
  const job = currentJob.value
  // A share scan's targets are the folders it reads. A volume scan's target is
  // the volume itself, which must not be presented as a shared folder.
  if (!job || job.scope === 'volume') return shares
  for (const target of job.targets) {
    if (!shares.some(share => share.path.path === target.path)) {
      shares.push({ path: target, name: { path: target.path.replace(/\/+$/, '').split('/').at(-1) ?? target.path } })
    }
  }
  return shares
})
const currentLocationLabel = computed(() => currentLocation(progress.value?.current_subvolume, locationShares.value))
const currentLocationFull = computed(() => progress.value?.current_subvolume?.path ?? '')
const currentMode = computed(() => modeForProfile(currentJob.value?.profile ?? 'low'))
const profileLabel = computed(() => t(currentMode.value === 'fast' ? 'scan.mode.fast' : 'scan.mode.gentle'))
/** The mode Change mode switches to (SPEC §4.2). */
const otherMode = computed<ScanMode>(() => (currentMode.value === 'fast' ? 'gentle' : 'fast'))
const pausedTooLong = computed(() => {
  const job = currentJob.value
  return Boolean(job && ['paused', 'interrupted'].includes(job.state) && now.value - Date.parse(job.updated) > 5 * 60_000)
})
const needsPauseWarning = computed(() => Boolean(currentJob.value?.progress
  && currentJob.value.progress.units_done === 0
  && currentJob.value.progress.elapsed_s >= 60))
/** Why the job paused (SPEC §6.2 scan.paused.reason.*); a reason without text reads as plain "paused". */
const pausedReason = computed(() => {
  const reason = currentJob.value?.reason
  return reason && PAUSE_REASONS.includes(reason) ? t(`scan.paused.reason.${reason}`) : t('scan.status.paused')
})
const errorCodes = computed(() => (result.value?.errors ?? []).map(error => ({ set: 'error', value: error.code })))
const checkpointUnusable = computed(() => currentJob.value?.result?.error_codes.some(code => ['checkpoint_unusable', 'job_incompatible'].includes(code)) ?? false)
const hasPreviousReport = computed(() => Boolean(selectedVolume.value?.reports))

watch(() => jobs.list, list => {
  const active = list.find(job => job.kind === 'scan' && isActiveJob(job))
  if (active && !trackedJob.value && !reportJob.value) trackedJobId.value = active.job_id
  else if (!trackedJobId.value) trackedJobId.value = list.find(job => job.kind === 'scan' && isVisiblePausedJob(job, now.value))?.job_id ?? null
}, { immediate: true })

watch(() => [currentJob.value?.job_id, currentJob.value?.state] as const, async ([id, state]) => {
  result.value = null
  resultError.value = ''
  if (!id || !state || !['completed', 'partial', 'failed'].includes(state) || handledResults.has(id)) return
  handledResults.add(id)
  resultLoading.value = true
  try {
    const jobResult = await api('job.result', { job_id: id })
    if (jobResult.command !== 'scan') {
      resultError.value = code('api', 'unknown')
      return
    }
    result.value = jobResult
    if (state === 'completed' && jobResult.completion === 'complete') {
      toast.success(t('scan.done.complete'))
      navigate('overview')
    } else if (state === 'partial' || jobResult.completion === 'partial') {
      toast.warning(t('scan.done.partial.title'), t('scan.done.partial.body'))
      navigate('overview')
    }
  } catch (error) {
    resultError.value = apiErrorText(error)
  } finally {
    resultLoading.value = false
  }
})

// A System partition scan that ends while followed opens its result.
watch(reportState, (state, before) => {
  if (state && before && reportDone.value && reportJob.value) openRootfsView(reportJob.value.job_id)
})

async function requestPause() {
  const job = currentJob.value
  if (!job || actionBusy.value) return
  if (needsPauseWarning.value) {
    pauseOpen.value = true
    return
  }
  await pause(job)
}

async function pause(job: Job) {
  await actions.pause(job)
  pauseOpen.value = false
}

async function cancel() {
  const job = reportJob.value ?? currentJob.value
  if (!job || actionBusy.value) return
  if (await actions.cancel(job)) cancelOpen.value = false
}

function openResume() {
  if (!currentJob.value) return
  actions.failure.value = ''
  actions.notice.value = ''
  resumeMode.value = modeForProfile(currentJob.value.profile)
  resumeOpen.value = true
}

async function resume() {
  const job = currentJob.value
  if (!job || actionBusy.value) return
  if (await actions.resume(job, resumeMode.value)) resumeOpen.value = false
}

async function freshScan() {
  if (!currentJob.value || actionBusy.value) return
  if (await actions.fresh(currentJob.value)) {
    resumeOpen.value = false
    nav.newScanOpen = false
  }
}

onMounted(() => {
  jobs.scanningPageOpen = true
  pollNow()
})
onUnmounted(() => {
  jobs.scanningPageOpen = false
})
</script>

<template>
  <main class="scanning-page">
    <StateView v-if="jobs.error" kind="error" :error="jobs.error" />
    <LoadingArea v-else-if="startingScan" :text="t('scan.starting')" />
    <section v-else-if="reportJob" class="scan-grid">
      <Surface class="progress-card">
        <ProgressRing :ratio="null" :label="t('systemScan.title')">
          <strong class="ring-count">{{ formatDuration(reportElapsed) }}</strong>
        </ProgressRing>
        <div class="scan-title">
          <h2>{{ reportTitle }}</h2>
          <span class="scan-eta">{{ reportPhase }}</span>
        </div>
        <Space wrap :spacing="10" align-items="stretch" justify-content="center" class="scan-actions">
          <CancelButton v-if="reportActive" :disabled="actionBusy" @click="cancelOpen = true">{{ t('scan.cancel') }}</CancelButton>
          <Button v-else-if="reportDone" variant="primary" @click="openRootfsView(reportJob.job_id)">{{ t('systemScan.viewResult') }}</Button>
          <Button v-else variant="primary" @click="checkRootfs">{{ t('common.retry') }}</Button>
        </Space>
      </Surface>
      <Surface class="phase-card">
        <h2>{{ t('systemScan.title') }}</h2>
        <dl class="report-facts" role="status">
          <div><dt>{{ t('systemScan.status') }}</dt><dd>{{ code('state', reportState ?? 'unknown') }}</dd></div>
          <div><dt>{{ t('systemScan.phase') }}</dt><dd>{{ reportPhase }}</dd></div>
          <div><dt>{{ t('scan.stats.elapsed') }}</dt><dd>{{ reportElapsed != null ? formatDuration(reportElapsed) : t('common.unknown') }}</dd></div>
        </dl>
        <p v-if="reportActive" class="report-note">{{ t('systemScan.countsUnavailable') }}</p>
        <Callout v-if="actionNotice" tone="warning" :title="actionNotice" />
        <Callout v-if="actionFailure" tone="danger" :title="t('scan.actionFailedTitle')">{{ actionFailure }}</Callout>
      </Surface>
    </section>
    <EmptyState v-else-if="!currentJob" :title="t('scan.idle.title')" :description="t('scan.idle.body')">
      <template #actions><Button variant="primary" @click="openNewScan">{{ t('scan.idle.action') }}</Button></template>
    </EmptyState>
    <template v-else>
      <Callout v-if="missingFinalResult" :dismissible="false" tone="warning" :title="t(currentJob.reason === 'not_started' ? 'common.job.notStarted' : 'common.job.interrupted')">
        <p v-if="progress">{{ t('common.job.lastKnown') }}</p>
        <template #actions>
          <Button v-if="currentJob.state === 'interrupted' && currentJob.compatible" :disabled="actionBusy" @click="openResume">{{ t('scan.resume') }}</Button>
          <Button :disabled="actionBusy" @click="!currentJob.compatible || checkpointUnusable ? freshScan() : openNewScan()">{{ t('scan.idle.action') }}</Button>
        </template>
      </Callout>
      <section v-else class="scan-grid">
        <Surface class="progress-card">
          <ProgressRing :ratio="ratio" :label="t('scan.progress.label')" :mode="currentMode" :density="fieldDensity" :paused="['paused', 'interrupted'].includes(currentJob.state)">
            <strong v-if="ratio != null" class="ring-percent" :class="{ 'ring-long': ringPercent.length > RING_LONG_TEXT }">{{ ringPercent }}</strong>
            <strong v-else class="ring-count" :class="{ 'ring-long': ringCount.length > RING_LONG_TEXT }">{{ ringCount }}</strong>
          </ProgressRing>
          <div class="scan-title">
            <h2>{{ t(statusKey, { target: scanTarget }) }}</h2>
            <span v-if="etaLabel" class="scan-eta">{{ etaLabel }}</span>
            <span
              v-if="currentLocationLabel" class="current-subvolume" :title="currentLocationFull"
              :aria-label="t('scan.currentSubvolume', { name: currentLocationFull })"
            >
              {{ t('scan.currentSubvolume', { name: currentLocationLabel }) }}
            </span>
          </div>
          <Space wrap :spacing="10" align-items="stretch" justify-content="center" class="scan-actions">
            <Button v-if="currentJob.state === 'running'" :disabled="actionBusy" @click="requestPause">{{ t('scan.pause') }}</Button>
            <Button v-if="currentJob.state === 'running' && !publishing" :disabled="actionBusy || actions.modePending.value" :loading="actions.modePending.value" @click="actions.changeMode(currentJob, otherMode)">
              {{ t(actions.modePending.value ? 'scan.changeMode.pending' : otherMode === 'fast' ? 'scan.changeMode.toFast' : 'scan.changeMode.toGentle') }}
            </Button>
            <Button v-if="['paused', 'interrupted'].includes(currentJob.state) && currentJob.compatible" variant="primary" :disabled="actionBusy" @click="openResume">{{ t('scan.resume') }}</Button>
            <Button v-else-if="['paused', 'interrupted'].includes(currentJob.state)" variant="primary" :disabled="actionBusy" @click="freshScan">{{ t('scan.idle.action') }}</Button>
            <CancelButton v-if="['running', 'pausing', 'cancelling', 'paused', 'interrupted'].includes(currentJob.state)" :disabled="actionBusy" @click="cancelOpen = true">{{ t('scan.cancel') }}</CancelButton>
          </Space>
          <p v-if="currentJob.state === 'running' && publishing" class="scan-eta">{{ t('scan.changeMode.publishing') }}</p>
        </Surface>

        <Surface class="phase-card">
          <h2>{{ t('scan.phases.title') }}</h2>
          <PhaseList :job="currentJob" :filesystem="selectedVolume?.filesystem" />
        </Surface>
      </section>

      <section class="stat-grid" :aria-label="t('scan.stats.title')">
        <Surface class="stat-card"><Statistic :label="t('scan.stats.entries')" :value="formatCount(progress?.entries)" /></Surface>
        <Surface v-if="selectedVolume?.filesystem !== 'ext4'" class="stat-card"><Statistic :label="t('scan.stats.bytes')" :value="formatBytes(progress?.bytes_read)" /></Surface>
        <Surface v-else class="stat-card"><Statistic :label="t('scan.stats.processed')" :value="formatCount(progress?.items)" :note="t('scan.stats.processedNote')" /></Surface>
        <Surface class="stat-card"><Statistic :label="t('scan.stats.elapsed')" :value="formatDuration(elapsed)" /></Surface>
        <Surface class="stat-card"><Statistic :label="t('scan.stats.mode')" :value="profileLabel" /></Surface>
      </section>

      <Callout v-if="currentJob.progress?.restart" tone="info" :title="t('scan.restart.title')">
        {{ t('scan.restart.body', { entries: formatCount(currentJob.progress.restart.entries), items: formatCount(currentJob.progress.restart.items) }) }}
      </Callout>
      <Callout v-if="!currentJob.compatible && ['paused', 'interrupted', 'failed'].includes(currentJob.state)" tone="warning" :title="t('scan.upgrade.rescan')" />
      <Callout v-if="currentJob.state === 'paused'" tone="warning" :title="pausedReason">
        <p>{{ t('scan.paused.body', { time: formatDateTime(currentJob.updated), count: currentJob.units }) }}</p>
        <p v-if="pausedTooLong" class="resume-hint">{{ t('scan.paused.restartHint') }}</p>
      </Callout>
      <Callout v-else-if="currentJob.state === 'interrupted'" tone="warning" :title="t('scan.status.interrupted')">
        <p>{{ t('scan.interrupted', { time: formatDateTime(currentJob.updated) }) }}</p>
        <p v-if="pausedTooLong" class="resume-hint">{{ t('scan.paused.restartHint') }}</p>
      </Callout>
      <Callout v-else-if="state === 'pausing' || state === 'cancelling'" :title="t(statusKey, { target: scanTarget })">
        {{ t('scan.actionPending') }}
      </Callout>
      <Callout v-if="currentJob.request_ignored === 'pause'" tone="warning" :title="t('scan.pause.tooLate')" />
      <Callout v-if="currentJob.request_ignored === 'cancel'" tone="warning" :title="t('scan.cancel.tooLate')" />
      <LoadingArea v-if="resultLoading" :text="t('scan.result.loading')" />
      <StateView v-if="currentJob.state === 'partial' || result?.completion === 'partial'" kind="partial" :codes="errorCodes" />
      <Callout v-else-if="checkpointUnusable" tone="warning" :title="t('scan.checkpoint.title')">
        <p>{{ t('scan.checkpoint.body') }}</p>
        <Button :disabled="actionBusy" @click="freshScan">{{ t('scan.idle.action') }}</Button>
      </Callout>
      <StateView v-else-if="currentJob.state === 'failed' || resultError" kind="error" :codes="errorCodes" />
      <Callout v-if="currentJob.state === 'cancelled'" :title="t('scan.cancel.done')" />
      <Callout v-if="actionNotice" tone="warning" :title="actionNotice" />
      <Callout v-if="actionFailure" tone="danger" :title="t('scan.actionFailedTitle')">{{ actionFailure }}</Callout>

      <Callout v-if="hasPreviousReport" :title="t('scan.previous.title')">
        <span>{{ t('scan.previous.body', { time: formatRelative(selectedVolume?.last_scan) }) }}</span>
        <template #actions>
          <Button variant="ghost" @click="navigate('overview')">{{ t('scan.previous.view') }}</Button>
        </template>
      </Callout>
      <Callout v-else :title="t('scan.previous.none')">{{ t('scan.previous.noneBody') }}</Callout>
    </template>
  </main>

  <Dialog v-model:open="pauseOpen" :aria-describedby="undefined" :title="t('scan.pause.confirmTitle')" :close-label="t('common.close')">
    <p>{{ t('scan.pause.confirmBig') }}</p>
    <template #footer>
      <Button @click="pauseOpen = false">{{ t('common.cancel') }}</Button>
      <Button variant="primary" :loading="actionBusy" @click="currentJob && pause(currentJob)">{{ t('scan.pause') }}</Button>
    </template>
  </Dialog>

  <Dialog v-model:open="cancelOpen" :title="t('scan.cancel.title')" :description="t(reportJob ? 'systemScan.cancelBody' : hasPreviousReport ? 'scan.cancel.bodyKeep' : 'scan.cancel.body')" :close-label="t('common.close')">
    <template #footer>
      <Button @click="cancelOpen = false">{{ t('scan.cancel.keep') }}</Button>
      <Button variant="danger" :loading="actionBusy" @click="cancel">{{ t('scan.cancel.confirm') }}</Button>
    </template>
  </Dialog>

  <Dialog v-model:open="resumeOpen" :title="t('scan.resume.title')" :description="t('scan.resume.body')" :close-label="t('common.close')">
    <Callout v-if="pausedTooLong" tone="warning" :title="t('scan.paused.restartHint')" />
    <Callout v-if="actionFailure" tone="danger" :title="t('scan.actionFailedTitle')">
      <p>{{ actionFailure }}</p>
      <Button :disabled="actionBusy" @click="freshScan">{{ t('scan.idle.action') }}</Button>
    </Callout>
    <Callout v-if="actionNotice" tone="warning" :title="actionNotice" />
    <RadioGroup v-model="resumeMode" :aria-label="t('scan.stats.mode')" :options="resumeModeRadios" />
    <template #footer>
      <Button @click="resumeOpen = false">{{ t('common.cancel') }}</Button>
      <Button variant="primary" :loading="actionBusy" @click="resume">{{ t('scan.resume') }}</Button>
    </template>
  </Dialog>
</template>

<style scoped>
.scanning-page { display: grid; align-content: start; gap: var(--ui-section-gap); }
.scan-grid { display: grid; grid-template-columns: minmax(330px, .9fr) minmax(400px, 1.1fr); gap: 20px; }
.progress-card, .phase-card { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 18px; min-height: 470px; padding: 26px; }
.phase-card { align-items: stretch; justify-content: flex-start; gap: 20px; padding: 26px 30px; }
.phase-card h2 { color: var(--ui-text); font-size: var(--ui-font-size-lg); font-weight: 700; }
.ring-percent, .ring-count { max-width: 140px; color: var(--ui-text); font-size: 28px; font-weight: 600; font-variant-numeric: tabular-nums; white-space: nowrap; }
.ring-percent.ring-long, .ring-count.ring-long { font-size: 22px; }
.scan-eta { max-width: 260px; color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); line-height: 1.4; }
.scan-title { display: grid; gap: 4px; text-align: center; }
.scan-title h2 { color: var(--ui-text); font-size: 17px; font-weight: 700; }
.current-subvolume { display: block; max-width: 320px; overflow: hidden; color: var(--ui-text-muted); font-size: var(--ui-font-size-sm); text-overflow: ellipsis; white-space: nowrap; }

.report-facts { display: grid; gap: 12px; margin: 0; }
.report-facts > div { display: grid; grid-template-columns: minmax(90px, 1fr) minmax(0, 2fr); gap: 12px; }
.report-facts dt, .report-note { color: var(--ui-text-secondary); }
.report-facts dd { margin: 0; overflow-wrap: anywhere; font-variant-numeric: tabular-nums; }
.report-note { margin: 0; font-size: var(--ui-font-size-sm); }
.stat-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; }
.stat-card { padding: 16px 18px; }
.resume-hint { margin-top: 6px; }
@media (max-width: 900px) { .scan-grid { grid-template-columns: 1fr; } .progress-card { min-height: 370px; } }
@media (max-width: 650px) { .stat-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>
