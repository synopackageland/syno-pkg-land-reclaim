<script setup lang="ts">
// One running job in the shell's status list: what runs, where it is, and one
// way into it. Pausing and cancelling live in the job's own progress view, so
// a row never offers more than View progress.
import { computed, ref } from 'vue'
import { Button, Dialog, ListItem, Progress } from '@synopackageland/ui'
import type { Job } from '@/types'
import { code, formatCount, formatDuration, joinParts, locale, t } from '@/i18n'
import { shownState } from '@/stores/jobs'
import { openJob as followJob } from '@/stores/nav'
import CancelButton from './CancelButton.vue'
import { useJobActions } from './jobActions'
import { normalizedPhase, progressRatio } from './scanProgress'
import { scopeLabel } from './scanTarget'
import { isRootfsApplyJob, progressRatio as rootfsProgressRatio, rootfsToolKey } from '@/pages/RootfsApply/rootfsApply'
import type { RootfsApplyJob } from '@/pages/RootfsApply/rootfsTypes'

const props = defineProps<{ job: Job }>()
const cancelOpen = ref(false)
const planningDetails = ref(false)
const reportJob = computed(() => props.job.kind === 'rootfs-report')
const reportPhase = computed(() => props.job.state === 'queued' ? t('systemScan.waiting') : props.job.state === 'running' ? t('systemScan.reading') : code('state', shownState(props.job)))
/** A cleanup or System cleanup plan: short work shown in a dialog of its own. */
const planningJob = computed(() => ['cleanup-plan', 'rootfs-plan'].includes(props.job.kind))
const actions = useJobActions()
const { pending } = actions
const actionError = computed(() => actions.notice.value || (actions.failure.value && t('scan.actionFailed', { code: actions.failure.value })))
const cleanupJob = computed(() => props.job.kind === 'cleanup-apply')
const rootfsJob = computed(() => isRootfsApplyJob(props.job) ? props.job as unknown as RootfsApplyJob : null)
/** A move's items done of its plan's, once the job reports them. */
const cleanupProgress = computed(() => (props.job as { cleanup?: { items_done: number; items_total: number } | null }).cleanup ?? null)
const cleanupRatio = computed(() => cleanupProgress.value?.items_total ? cleanupProgress.value.items_done / cleanupProgress.value.items_total : null)
const ratio = computed(() => rootfsJob.value
  ? rootfsProgressRatio(rootfsJob.value.rootfs)
  : cleanupJob.value ? cleanupRatio.value : reportJob.value ? null : progressRatio(props.job.progress))
const phase = computed(() => {
  if (reportJob.value) return reportPhase.value
  if (planningJob.value) return t('rootfsClean.state.checking')
  if (cleanupJob.value) return t('review.applying.title')
  const rootfs = rootfsJob.value?.rootfs
  if (rootfs?.phase === 'tools') {
    const tool = rootfs.tool
    return tool
      ? t('rootfsApply.progress.toolsNamed', { tool: t(rootfsToolKey(tool), { tool }) })
      : t('rootfsApply.progress.tools')
  }
  if (rootfs?.phase === 'moves') {
    return t('rootfsApply.progress.moving', {
      done: formatCount(rootfs.items_done),
      total: formatCount(rootfs.items_total),
    })
  }
  return rootfsJob.value
    ? t('rootfsApply.progress.title')
    : t(`scan.phase.${normalizedPhase(props.job.progress?.phase ?? props.job.phase)}`)
})

const location = computed(() => reportJob.value
  ? joinParts([t('rootfs.name'), code('state', shownState(props.job))], locale)
  : planningJob.value
  ? t(`common.job.kind.${props.job.kind}`)
  : rootfsJob.value
  ? t('rootfsApply.progress.title')
  : cleanupJob.value
    ? t('common.job.kind.cleanup-apply')
    : joinParts([scopeLabel(props.job.targets, props.job.scope === 'volume'), code('state', shownState(props.job))], locale))
const phaseDetail = computed(() => reportJob.value
  ? joinParts([reportPhase.value, props.job.elapsed_s != null ? formatDuration(props.job.elapsed_s) : ''], locale)
  : planningJob.value
  ? code('state', shownState(props.job))
  : cleanupJob.value
  ? joinParts([
      t(`review.jobState.${shownState(props.job)}`),
      ...(cleanupProgress.value ? [t('review.applying.count', { done: formatCount(cleanupProgress.value.items_done), total: formatCount(cleanupProgress.value.items_total) })] : []),
    ], locale)
  : rootfsJob.value
    ? phase.value
    : joinParts([phase.value, t('scan.progress.items', { entries: formatCount(props.job.progress?.entries) })], locale))

async function cancel() {
  if (await actions.cancel(props.job)) cancelOpen.value = false
}

function openJob() {
  if (planningJob.value) planningDetails.value = true
  else followJob(props.job)
}
</script>

<template>
  <ListItem class="job-strip" :title="location" :description="phaseDetail">
    <Progress v-if="ratio != null" class="job-progress" :value="ratio * 100" :max="100" :label="phase" />
    <template #actions>
      <Button size="sm" @click="openJob">{{ t('newScan.busy.view') }}</Button>
    </template>
  </ListItem>
  <Dialog v-if="planningJob" v-model:open="planningDetails" :aria-describedby="undefined" :title="location" :close-label="t('common.close')">
    <div class="job-details" role="status">
      <dl>
        <div><dt>{{ t('systemScan.status') }}</dt><dd>{{ code('state', shownState(job)) }}</dd></div>
        <div><dt>{{ t('systemScan.phase') }}</dt><dd>{{ phaseDetail }}</dd></div>
        <div><dt>{{ t('scan.stats.elapsed') }}</dt><dd>{{ job.elapsed_s != null ? formatDuration(job.elapsed_s) : t('common.unknown') }}</dd></div>
      </dl>
      <p v-if="actionError" role="alert" class="job-error">{{ actionError }}</p>
    </div>
    <template #footer>
      <CancelButton v-if="['queued', 'running'].includes(job.state)" :disabled="pending !== null" @click="cancelOpen = true">{{ t('scan.cancel') }}</CancelButton>
      <Button @click="planningDetails = false">{{ t('common.close') }}</Button>
    </template>
  </Dialog>
  <Dialog v-model:open="cancelOpen" :title="t('scan.cancel')" :description="t('common.job.planning.cancelBody')" :close-label="t('common.close')">
    <template #footer>
      <Button @click="cancelOpen = false">{{ t('scan.cancel.keep') }}</Button>
      <Button variant="danger" :loading="pending === 'cancel'" @click="cancel">{{ t('scan.cancel.confirm') }}</Button>
    </template>
  </Dialog>
</template>

<style scoped>
.job-progress { max-width: 360px; margin-top: 4px; }
.job-error { margin: 0; color: var(--ui-danger); font-size: var(--ui-font-size-sm); }
.job-details { display: grid; gap: 12px; }
.job-details dl { display: grid; gap: 12px; margin: 0; }
.job-details dl > div { display: grid; grid-template-columns: minmax(90px, 1fr) minmax(0, 2fr); gap: 12px; }
.job-details dt { color: var(--ui-text-secondary); }
.job-details dd { margin: 0; overflow-wrap: anywhere; font-variant-numeric: tabular-nums; }
</style>
