<script setup lang="ts">
import { runInBackground } from '@/api/planning'
// Body for S6/S7. U3 owns the dialog shell and hosts this component for step 3.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { Badge, Button, Callout, PathDisplay, Progress, LoadingArea, Statistic, Surface, Tooltip, TypoA, Space, useToast } from '@synopackageland/ui'
import { labelSeparator } from '@synopackageland/sdk'
import { formatBytes, formatCount, formatPercent, locale, t } from '@/i18n'
import { UPDATE_ROOM_BYTES } from '@/pages/Overview/systemPartition'
import { api, ApiError } from '@/api'
import type { RootfsReportDocument } from '@/types'
import RootfsHistorySection from './RootfsHistorySection.vue'
import { invalidateAnswers } from '@/stores/answers'
import { authorizeRootWork } from '@/stores/authorization'
import { findSubmittedJob } from '@/stores/jobRecovery'
import { jobs, refreshJobs } from '@/stores/jobs'
import {
  hasFailedTools,
  isPausedResult,
  isRootfsApplyJob,
  levelAfterApply,
  progressRatio,
  rootfsCategoryKey,
  rootfsOutcomeKey,
  rootfsSkipKey,
  rootfsToolKey,
  skippedByReason,
} from './rootfsApply'
import type { RootfsApplyJob, RootfsApplyResult, RootfsHistoryRecord } from './rootfsTypes'
import { refusalMessageKey } from '@/rootfs/cleanupModel'
import { fileStationBlock, openInFileStation } from '@/components/fileStation'

const props = withDefaults(defineProps<{
  open: boolean
  mode?: 'standard' | 'deep'
  /** The confirmed S5 plan. Omit when the host supplies an existing job id. */
  planId?: string | null
  /** Existing rootfs-apply job, e.g. when opened from the global job strip. */
  jobId?: string | null
  /** Follow an existing job without submitting rootfs.apply. */
  followOnly?: boolean
}>(), { mode: 'standard', planId: null, jobId: null, followOnly: false })

const emit = defineEmits<{
  close: []
  complete: [result: RootfsApplyResult]
  'check-again': [report: RootfsReportDocument | null]
}>()

type ViewState = 'loading' | 'progress' | 'result' | 'interrupted' | 'error'
const viewState = ref<ViewState>('loading')
const error = ref(false)
const currentJob = ref<RootfsApplyJob | null>(null)
const result = ref<RootfsApplyResult | null>(null)
const latestReport = ref<RootfsReportDocument | null>(null)
const historyApplies = ref<RootfsHistoryRecord[]>([])
const historyError = ref(false)
const reportError = ref(false)
const failureMessageKey = ref('rootfsApply.error.body')
const stopRequested = ref(false)
const actionPending = ref(false)
const planRejected = ref(false)
const followedPlanId = ref<string | null>(null)
const recoveryPlanId = computed(() => currentJob.value?.rootfs_plan?.plan_id ?? currentJob.value?.plan_id ?? followedPlanId.value)
const canContinue = computed(() => !!recoveryPlanId.value && !planRejected.value && currentJob.value?.reason !== 'not_started')
let timer: ReturnType<typeof setTimeout> | undefined
let operation = 0
let reportController = new AbortController()

const jobFromList = computed(() => {
  const wanted = currentJob.value?.job_id || props.jobId
  const candidates = (jobs.list as unknown as unknown[]).filter(isRootfsApplyJob)
  if (wanted) return candidates.find(job => job.job_id === wanted) ?? null
  return candidates.find(job => ['queued', 'running', 'pausing', 'cancelling'].includes(job.state))
    ?? null
})
const progress = computed(() => jobFromList.value?.rootfs ?? currentJob.value?.rootfs)
const ratio = computed(() => progressRatio(progress.value))
const progressText = computed(() => {
  if (progress.value?.phase === 'tools') {
    const tool = progress.value.tool
    return tool
      ? t('rootfsApply.progress.toolsNamed', { tool: t(rootfsToolKey(tool), { tool }) })
      : t('rootfsApply.progress.tools')
  }
  return t('rootfsApply.progress.moving', {
    done: formatCount(progress.value?.items_done),
    total: formatCount(progress.value?.items_total),
  })
})
const canStop = computed(() => viewState.value === 'progress'
  && jobFromList.value?.state === 'running'
  && progress.value?.phase !== 'tools'
  && !stopRequested.value
  && !actionPending.value)
const skippedCounts = computed(() => skippedByReason(result.value))
const normalAgain = computed(() => levelAfterApply(latestReport.value) === 'ok')
const availableAfter = computed(() => result.value?.root?.available_after_bytes ?? latestReport.value?.filesystem.available_bytes ?? null)
const enoughForUpdate = computed(() => availableAfter.value != null && availableAfter.value >= UPDATE_ROOM_BYTES)
const stillShort = computed(() => availableAfter.value != null && availableAfter.value < UPDATE_ROOM_BYTES)
const completionFailed = computed(() => result.value != null
  && !['complete', 'completed', 'partial', 'paused'].includes(result.value.completion))
const usedPercent = computed(() => {
  const filesystem = latestReport.value?.filesystem
  return filesystem && filesystem.total_bytes > 0
    ? (filesystem.total_bytes - filesystem.available_bytes) / filesystem.total_bytes
    : null
})
const showNotFreed = computed(() => result.value?.root?.freed_bytes != null
  && result.value.root.freed_bytes <= 0
  && (result.value?.moved?.files ?? 0) > 0)
const toolsFailed = computed(() => hasFailedTools(result.value))
const smallPartition = computed(() => stillShort.value
  && (latestReport.value?.filesystem.total_bytes ?? Infinity) < 3_000_000_000)
// Only owners with a real next step: a package frees space in Package Center.
const visibleOwners = computed(() => (latestReport.value?.report_only ?? []).filter(owner => owner.advice.code === 'package_data').slice(0, 3))
const openDeleted = computed(() => latestReport.value?.open_deleted?.bytes_estimate ?? 0)
const hasStillShortGuidance = computed(() => visibleOwners.value.length > 0 || smallPartition.value || openDeleted.value > 0)
const title = computed(() => {
  if (result.value?.completion === 'paused') return t('rootfsApply.result.stoppedTitle')
  if (result.value?.completion === 'partial') return t('rootfsApply.result.partialTitle')
  if (completionFailed.value) return t('rootfsApply.result.failedTitle')
  return t('rootfsApply.result.doneTitle')
})
const summary = computed(() => {
  const completed = result.value
  if (!completed) return ''
  if (completed.completion === 'paused') {
    return t('rootfsApply.result.stopped', {
      done: formatCount(completed.moved?.files ?? 0),
      total: formatCount(progress.value?.items_total),
    })
  }
  if (completionFailed.value) return t(failureMessageKey.value === 'rootfsApply.error.body' ? 'rootfsApply.result.failed' : failureMessageKey.value)
  return t(completed.completion === 'partial' ? 'rootfsApply.result.partial' : 'rootfsApply.result.done', {
    bytes: formatBytes(completed.root?.freed_bytes ?? null),
  })
})
const movePath = computed(() => result.value?.move_dir?.path ?? currentJob.value?.rootfs_plan?.move_dir.path ?? '')
const toast = useToast()
/** File Station shows only shared folders; a move folder elsewhere keeps its button disabled. */
const moveFolderBlocked = computed(() => movePath.value ? fileStationBlock({ path: movePath.value, kind: 'dir' }) != null : true)

function storageKey(planId: string | null | undefined) {
  return `synoreclaim.rootfsApply.${planId || 'active'}`
}

function stopTimer() {
  clearTimeout(timer)
  timer = undefined
}

function setApplyError(error: unknown) {
  const refusal = error instanceof ApiError ? String(error.detail?.stderr_code ?? error.code) : ''
  if (['plan_stale', 'plan_invalid', 'destination_changed', 'root_changed'].includes(refusal)) {
    planRejected.value = true
    failureMessageKey.value = refusalMessageKey(refusal)
    return
  }
  failureMessageKey.value = error instanceof ApiError && ['busy', 'backend_busy'].includes(error.code)
    ? 'rootfsClean.error.rootfsBusy'
    : 'rootfsApply.error.body'
}

function setResultError(document: RootfsApplyResult) {
  if (document.completion !== 'failed' && document.completion !== 'busy') return
  const firstError = document.errors[0]
  const messageKey = refusalMessageKey(firstError?.code, firstError?.holder?.command)
  if (messageKey !== 'rootfsClean.error.unknown') failureMessageKey.value = messageKey
}

function schedulePoll(requestId: number) {
  stopTimer()
  if (!props.open || document.hidden || requestId !== operation) return
  timer = setTimeout(() => void pollJob(requestId), 1500)
}

async function loadResult(jobId: string, requestId: number) {
  stopTimer()
  viewState.value = 'loading'
  try {
    const document = await api('job.result', { job_id: jobId })
    if (requestId !== operation) return
    if (document.command !== 'rootfs-apply') throw new Error('job.result is not a rootfs apply result')
    setResultError(document)
    planRejected.value = document.errors.some(error => ['plan_stale', 'plan_invalid', 'destination_changed', 'root_changed'].includes(error.code))
    result.value = document
    viewState.value = 'result'
    stopRequested.value = false
    try {
      const report = await runInBackground('rootfs.report', { max_age_s: 0 }, { signal: reportController.signal })
      latestReport.value = report.completion === 'failed' ? null : report
      reportError.value = report.completion === 'failed'
    } catch {
      latestReport.value = null
      reportError.value = true
    }
    try {
      const history = await api('rootfs.history', {})
      historyApplies.value = history.applies
      historyError.value = false
    } catch {
      historyApplies.value = []
      historyError.value = true
    }
    if (requestId === operation) emit('complete', document)
    // The apply moved bytes and changed the partition: answers must be read again.
    invalidateAnswers()
  } catch {
    if (requestId !== operation) return
    // Hard interruptions need not publish a final result. The persisted
    // progress is only the last known checkpoint, not a completed total.
    result.value = null
    viewState.value = 'interrupted'
    stopRequested.value = false
    stopTimer()
    // An interrupted apply may already have moved bytes: read again as well.
    invalidateAnswers()
  }
}

async function pollJob(requestId: number) {
  if (!props.open || requestId !== operation) return
  await refreshJobs()
  if (requestId !== operation) return
  if (jobs.error) {
    error.value = true
    viewState.value = 'error'
    stopTimer()
    return
  }
  const job = jobFromList.value
  if (!job) {
    const existingId = currentJob.value?.job_id || props.jobId
    if (existingId) {
      await loadResult(existingId, requestId)
    } else {
      error.value = true
      viewState.value = 'error'
    }
    return
  }
  currentJob.value = job
  if (['queued', 'running', 'pausing', 'cancelling'].includes(job.state)) {
    viewState.value = 'progress'
    schedulePoll(requestId)
    return
  }
  await loadResult(job.job_id, requestId)
}

async function submitApply(requestId: number) {
  if (!props.planId) {
    error.value = true
    viewState.value = 'error'
    return
  }
  viewState.value = 'loading'
  error.value = false
  let knownJobIds: string[] = []
  let submitted = false
  try {
    knownJobIds = (await api('jobs.list', {})).jobs.map(job => job.job_id)
    submitted = true
    const started = await api('rootfs.apply', { plan_id: props.planId, confirm_no_restore: true })
    if (requestId !== operation) return
    if (started.command === 'rootfs-apply') {
      result.value = started
      setResultError(started)
      planRejected.value = true
      viewState.value = 'result'
      return
    }
    if (!started.job_id) throw new Error('rootfs.apply did not queue a job')
    followedPlanId.value = props.planId
    currentJob.value = { job_id: started.job_id, kind: 'rootfs-apply', plan_id: props.planId, state: 'queued' }
    sessionStorage.setItem(storageKey(props.planId), started.job_id)
    await pollJob(requestId)
  } catch (failure) {
    if (requestId !== operation) return
    if (submitted) {
      try {
        const listed = await api('jobs.list', {})
        const recovered = findSubmittedJob(listed.jobs, 'rootfs-apply', props.planId, knownJobIds)
        if (recovered) {
          currentJob.value = recovered as unknown as RootfsApplyJob
          sessionStorage.setItem(storageKey(props.planId), recovered.job_id)
          await pollJob(requestId)
          return
        }
      } catch { /* The next app opening can rediscover jobs; never resend apply. */ }
    }
    setApplyError(failure)
    error.value = true
    viewState.value = 'error'
  }
}

async function openStep() {
  reportController.abort()
  reportController = new AbortController()
  stopTimer()
  const requestId = ++operation
  result.value = null
  latestReport.value = null
  historyApplies.value = []
  historyError.value = false
  reportError.value = false
  failureMessageKey.value = 'rootfsApply.error.body'
  planRejected.value = false
  followedPlanId.value = null
  error.value = false
  stopRequested.value = false
  viewState.value = 'loading'

  const stored = props.jobId ?? sessionStorage.getItem(storageKey(props.planId))
  if (stored) {
    // Only storage keyed by this confirmed plan proves the association.
    // A followed older job without metadata must be rechecked.
    followedPlanId.value = props.jobId ? null : props.planId
    currentJob.value = { job_id: stored, kind: 'rootfs-apply', state: 'queued' }
    await pollJob(requestId)
    return
  }

  await refreshJobs()
  if (jobs.error) {
    error.value = true
    viewState.value = 'error'
    return
  }
  const active = (jobs.list as unknown as unknown[]).filter(isRootfsApplyJob)
    .find(job => ['queued', 'running', 'pausing', 'cancelling'].includes(job.state))
  if (active) {
    currentJob.value = active
    sessionStorage.setItem(storageKey(props.planId), active.job_id)
    await pollJob(requestId)
  } else if (props.followOnly) {
    error.value = true
    viewState.value = 'error'
  } else {
    await submitApply(requestId)
  }
}

async function stopAfterBatch() {
  const job = jobFromList.value
  if (!job || actionPending.value) return
  actionPending.value = true
  error.value = false
  try {
    if (!(await authorizeRootWork())) return
    await api('job.cancel', { job_id: job.job_id })
    stopRequested.value = true
    await refreshJobs()
    if (['queued', 'running', 'pausing', 'cancelling'].includes(jobFromList.value?.state ?? '')) schedulePoll(operation)
    else await pollJob(operation)
  } catch (failure) {
    setApplyError(failure)
    error.value = true
  } finally {
    actionPending.value = false
  }
}

async function continueApply() {
  const planId = recoveryPlanId.value ?? null
  if (!planId || !canContinue.value || actionPending.value) return
  actionPending.value = true
  if (!(await authorizeRootWork())) {
    actionPending.value = false
    return
  }
  stopTimer()
  viewState.value = 'loading'
  result.value = null
  latestReport.value = null
  historyApplies.value = []
  historyError.value = false
  error.value = false
  stopRequested.value = false
  const requestId = ++operation
  try {
    const started = await api('rootfs.apply', { plan_id: planId, confirm_no_restore: true })
    if (started.command === 'rootfs-apply') {
      result.value = started
      setResultError(started)
      planRejected.value = true
      viewState.value = 'result'
      return
    }
    if (!started.job_id) throw new Error('rootfs.apply did not queue a job')
    followedPlanId.value = planId
    currentJob.value = { job_id: started.job_id, kind: 'rootfs-apply', plan_id: planId, state: 'queued' }
    sessionStorage.setItem(storageKey(planId), started.job_id)
    await pollJob(requestId)
  } catch (failure) {
    setApplyError(failure)
    error.value = true
    viewState.value = 'error'
  } finally {
    actionPending.value = false
  }
}

async function checkAgain() {
  let report: RootfsReportDocument | null = null
  reportError.value = false
  try {
    const response = await runInBackground('rootfs.report', { max_age_s: 0 }, { signal: reportController.signal })
    report = response.completion === 'failed' ? null : response
    latestReport.value = report
  } catch {
    latestReport.value = null
    reportError.value = true
  }
  emit('check-again', report)
}

function openMoveFolder() {
  if (!openInFileStation({ path: movePath.value, kind: 'dir' })) toast.info(t('common.fileStation.failed'))
}

watch(() => props.open, open => {
  if (open) void openStep()
  else {
    ++operation
    reportController.abort()
    stopTimer()
  }
}, { immediate: true })

watch(() => [props.planId, props.jobId], () => {
  if (props.open) void openStep()
})

onBeforeUnmount(() => {
  reportController.abort()
  ++operation
  stopTimer()
})
</script>

<template>
  <section class="rootfs-apply" aria-live="polite">
    <LoadingArea v-if="viewState === 'loading'" :text="t('rootfsApply.loading')" />

    <div v-else-if="viewState === 'error'" class="result-state">
      <Callout tone="danger" :title="t('rootfsApply.error.title')">
        <p>{{ t(failureMessageKey) }}</p>
      </Callout>
      <Space wrap :spacing="8" align-items="stretch" justify-content="end" class="actions"><Button kind="action" @click="checkAgain">{{ t('rootfsApply.result.checkAgain') }}</Button><Button kind="action" @click="emit('close')">{{ t('common.close') }}</Button></Space>
    </div>

    <div v-else-if="viewState === 'interrupted'" class="result-state">
      <Callout tone="warning" :title="t(currentJob?.reason === 'not_started' ? 'common.job.notStarted' : 'common.job.interrupted')">
        <p v-if="progress">{{ t('common.job.lastKnown') }}</p>
        <p v-if="progress">{{ progressText }}</p>
        <div v-if="movePath && currentJob?.reason !== 'not_started'" class="moved-to">
          <span>{{ t('rootfsApply.result.movedTo') }}{{ labelSeparator(locale) }}</span><PathDisplay :path="movePath" />
        </div>
      </Callout>
      <Space wrap :spacing="8" align-items="stretch" justify-content="end" class="actions">
        <Button v-if="canContinue" kind="action" :loading="actionPending" variant="primary" @click="continueApply">{{ t('rootfsApply.result.continue') }}</Button>
        <Button kind="action" @click="checkAgain">{{ t('rootfsApply.result.checkAgain') }}</Button>
        <Button kind="action" @click="emit('close')">{{ t('common.close') }}</Button>
      </Space>
    </div>

    <div v-else-if="viewState === 'progress'" class="progress-state">
      <p>{{ t(`rootfsClean.mode.${currentJob?.rootfs_plan?.mode ?? props.mode}`) }}</p>
      <h2>{{ t('rootfsApply.progress.title') }}</h2>
      <Surface class="progress-card">
        <Progress v-if="ratio != null" :value="ratio * 100" :max="100" :label="progressText" />
        <div v-if="progress?.phase === 'moves'" class="progress-copy">
          <span>{{ formatBytes(progress.bytes_done) }} / {{ formatBytes(progress.bytes_total) }}</span>
        </div>
        <p>{{ t('rootfsApply.progress.how') }}</p>
      </Surface>
      <Callout tone="info">{{ t('rootfsApply.progress.close') }}</Callout>
      <Callout v-if="stopRequested" tone="warning">{{ t('rootfsApply.progress.stopPending') }}</Callout>
      <Callout v-if="error" tone="danger">{{ t('rootfsApply.error.stopFailed') }}</Callout>
      <Space wrap :spacing="8" align-items="stretch" justify-content="end" class="actions">
        <Button v-if="canStop" kind="action" variant="danger" :loading="actionPending" @click="stopAfterBatch">
          {{ t('rootfsApply.progress.stop') }}
        </Button>
      </Space>
    </div>

    <div v-else-if="result" class="result-state">
      <p>{{ t(`rootfsClean.mode.${result.mode ?? currentJob?.rootfs_plan?.mode ?? props.mode}`) }}</p>
      <Callout :tone="result.completion === 'partial' || result.completion === 'paused' ? 'warning' : completionFailed || toolsFailed ? 'danger' : 'success'" :title="title">
        <p>{{ summary }}</p>
        <p v-if="normalAgain" class="normal-again">{{ t('rootfsApply.result.backToNormal') }}</p>
      </Callout>

      <div class="result-stats">
        <Surface class="stat-card">
          <Statistic
            :label="t('rootfsApply.result.freeNow')"
            :value="formatBytes(availableAfter)"
            :note="usedPercent != null ? `${formatPercent(usedPercent)} ${t('rootfsApply.result.used')}` : undefined"
          />
        </Surface>
        <Surface v-if="result.moved" class="stat-card">
          <Statistic
            :label="t('rootfsApply.result.moved')"
            :value="formatBytes(result.moved?.size_bytes ?? 0)"
            :note="t('rootfsApply.result.items', { count: formatCount(result.moved?.files ?? 0) })"
          />
        </Surface>
        <Surface v-if="movePath" class="stat-card location-card">
          <span>{{ t('rootfsApply.result.movedTo') }}</span>
          <strong class="move-folder">{{ movePath }}</strong>
          <small>{{ t('rootfsApply.result.readme') }}</small>
          <Tooltip :content="t('common.fileStation.notShared')" :disabled="!moveFolderBlocked">
            <span class="file-station-action" :tabindex="moveFolderBlocked ? 0 : undefined">
              <Button size="sm" variant="ghost" :disabled="moveFolderBlocked" @click="openMoveFolder">{{ t('rootfsApply.result.open') }}</Button>
            </span>
          </Tooltip>
        </Surface>
      </div>

      <Callout v-if="enoughForUpdate" tone="success">
        {{ t('rootfsApply.result.enoughForUpdate', { free: formatBytes(availableAfter), typical: formatBytes(UPDATE_ROOM_BYTES) }) }}
      </Callout>
      <Callout v-else-if="stillShort" tone="warning">
        {{ t('rootfsApply.result.notEnoughForUpdate', { free: formatBytes(availableAfter), typical: formatBytes(UPDATE_ROOM_BYTES) }) }}
      </Callout>
      <Callout v-for="action in result.follow_up ?? []" :key="action" tone="warning">
        {{ t(`rootfsClean.followUp.${action}`) }}
      </Callout>
      <Callout v-if="showNotFreed" tone="warning">{{ t('rootfsApply.result.notFreed') }}</Callout>
      <Callout v-if="toolsFailed" tone="danger" :title="t('rootfsApply.result.toolsFailed')">
        <ul class="detail-list">
          <li v-for="tool in (result.tools ?? []).filter(item => ['failed', 'killed', 'timed_out', 'untrusted', 'not_started'].includes(item.outcome))" :key="tool.tool">
            <span>{{ t(rootfsToolKey(tool.tool), { tool: tool.tool }) }}</span>
            <Badge tone="danger">{{ t(rootfsOutcomeKey(tool.outcome), { outcome: tool.outcome }) }}</Badge>
          </li>
        </ul>
      </Callout>

      <Surface v-if="skippedCounts.length" class="detail-card">
        <h3>{{ t('rootfsApply.result.skipped') }}</h3>
        <ul class="detail-list">
          <li v-for="entry in skippedCounts" :key="entry.reason" class="skip-group">
            <div class="skip-summary">
              <span>{{ t(rootfsSkipKey(entry.reason), { reason: entry.reason }) }}</span>
              <strong>{{ formatCount(entry.count) }}</strong>
            </div>
            <ul class="skip-paths">
              <li v-for="(path, index) in entry.paths" :key="`${path}:${index}`"><code>{{ path }}</code></li>
            </ul>
          </li>
        </ul>
      </Surface>

      <Surface v-if="stillShort && hasStillShortGuidance" class="detail-card">
        <h3 v-if="visibleOwners.length">{{ t('rootfsApply.result.tryNext') }}</h3>
        <ul v-if="visibleOwners.length" class="owner-list">
          <li v-for="owner in visibleOwners" :key="`${owner.category}:${owner.name}`">
            <strong>{{ owner.name ?? t(rootfsCategoryKey(owner.category), { name: '' }) }}</strong>
            <span>{{ formatBytes(owner.bytes) }}</span>
            <small>{{ t('rootfsApply.advice.package_data') }}</small>
          </li>
        </ul>
        <TypoA v-if="smallPartition" class="kb-link" :href="`https://kb.synology.com/${locale === 'zh-TW' ? 'zh-tw' : 'en-global'}/DSM/tutorial/DSM_update_insufficient_system_capacity_for_update`" target="_blank" rel="noreferrer">
          {{ t('rootfsApply.result.smallPartition') }}
        </TypoA>
        <p v-if="openDeleted > 0" class="open-deleted">
          {{ t('rootfsApply.result.openDeleted', { bytes: formatBytes(openDeleted), program: latestReport?.open_deleted?.holders[0]?.comm ?? t('rootfsApply.unknownProgram') }) }}
        </p>
      </Surface>

      <Surface v-if="result.tools?.length" class="detail-card">
        <h3>{{ t('rootfsApply.result.dsmCleanup') }}</h3>
        <ul class="detail-list">
          <li v-for="tool in result.tools ?? []" :key="tool.tool">
            <span>{{ t(rootfsToolKey(tool.tool), { tool: tool.tool }) }}</span>
            <Badge :tone="tool.outcome === 'succeeded' ? 'success' : tool.outcome === 'not_needed' ? 'neutral' : 'warning'">
              {{ t(rootfsOutcomeKey(tool.outcome), { outcome: tool.outcome }) }}
            </Badge>
          </li>
        </ul>
      </Surface>

      <Callout v-if="reportError" tone="warning">{{ t('rootfsApply.error.report') }}</Callout>
      <Callout v-if="historyError" tone="warning">{{ t('rootfsApply.history.loadError') }}</Callout>
      <RootfsHistorySection :applies="historyApplies" :can-clean-up="false" />
      <Space wrap :spacing="8" align-items="stretch" justify-content="end" class="actions">
        <Button v-if="isPausedResult(result) && canContinue" kind="action" :loading="actionPending" variant="primary" @click="continueApply">
          {{ t('rootfsApply.result.continue') }}
        </Button>
        <Button v-if="!normalAgain" kind="action" variant="primary" @click="checkAgain">{{ t('rootfsApply.result.checkAgain') }}</Button>
        <Button kind="action" @click="emit('close')">{{ t('common.close') }}</Button>
      </Space>
    </div>
  </section>
</template>

<style scoped>
.rootfs-apply { display: flex; flex-direction: column; gap: 14px; min-width: 0; }
.progress-state, .result-state { display: flex; flex-direction: column; gap: 12px; min-width: 0; }
.progress-state h2 { font-size: var(--ui-font-size-lg); font-weight: 700; }
.progress-card { display: flex; flex-direction: column; gap: 14px; padding: 18px; }
.progress-copy { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; color: var(--ui-text); }
.progress-copy span, .progress-card p, .stat-card small { color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.normal-again { font-weight: 600; }
.result-stats { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) minmax(240px, 1.5fr); gap: 10px; }
.stat-card { display: flex; flex-direction: column; align-items: flex-start; gap: 6px; min-width: 0; padding: 14px; }
.stat-card > span { color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); }
.stat-card > strong { font-size: var(--ui-font-size-lg); font-weight: 700; }
.stat-card > small { line-height: 1.4; }
.file-station-action { display: inline-flex; border-radius: var(--ui-radius-sm); outline: none; }
.file-station-action:focus-visible { box-shadow: 0 0 0 2px var(--ui-focus-ring); }
.location-card .move-folder { overflow-wrap: anywhere; font-size: var(--ui-font-size-sm); font-weight: 600; }
.detail-card { padding: 14px; }
.detail-card h3 { margin: 0 0 10px; font-size: var(--ui-font-size); font-weight: 700; }
.detail-list, .owner-list { display: flex; flex-direction: column; gap: 7px; padding: 0; margin: 0; list-style: none; }
.detail-list li { display: flex; align-items: center; justify-content: space-between; gap: 10px; font-size: var(--ui-font-size-sm); }
.detail-list li.skip-group { display: block; padding-block: 5px; border-bottom: 1px solid var(--ui-border); }
.skip-summary { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.skip-paths { display: flex; flex-direction: column; gap: 4px; padding: 5px 0 0 16px; margin: 0; color: var(--ui-text-secondary); list-style: disc; }
.skip-paths code { overflow-wrap: anywhere; font-size: 11px; }
.owner-list li { display: grid; grid-template-columns: minmax(120px, 1fr) auto; gap: 3px 12px; padding-block: 6px; border-bottom: 1px solid var(--ui-border); }
.owner-list li:last-child { border-bottom: 0; }
.owner-list strong { overflow-wrap: anywhere; font-size: var(--ui-font-size-sm); }
.owner-list span { font-size: var(--ui-font-size-sm); font-weight: 600; }
.owner-list small { grid-column: 1 / -1; color: var(--ui-text-secondary); font-size: var(--ui-font-size-sm); line-height: 1.4; }
.kb-link { display: inline-block; margin-top: 10px; font-size: var(--ui-font-size-sm); }
.open-deleted { margin-top: 10px; font-size: var(--ui-font-size-sm); }

.moved-to { display: flex; align-items: center; min-width: 0; }
.moved-to > span { flex-shrink: 0; white-space: pre; }
@media (max-width: 760px) { .result-stats { grid-template-columns: 1fr 1fr; } .location-card { grid-column: 1 / -1; } }
</style>
