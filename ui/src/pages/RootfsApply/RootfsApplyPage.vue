<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { jobs, refreshJobs } from '@/stores/jobs'
import { nav, navigate, openRootfsView } from '@/stores/nav'
import RootfsApplyStep from './RootfsApplyStep.vue'
import { isRootfsApplyJob } from './rootfsApply'
import type { RootfsApplyJob } from './rootfsTypes'

// Keep the selected attempt after it leaves the active set. Removing its
// props at that moment would restart the child and lose the recovery view.
const trackedJob = ref<RootfsApplyJob | null>(null)
watch([() => jobs.list, () => nav.followedJobId], ([list, followedId]) => {
  const candidates = (list as unknown as unknown[]).filter(isRootfsApplyJob)
  const followed = candidates.find(job => job.job_id === followedId)
  if (followed && followedId !== trackedJob.value?.job_id) {
    trackedJob.value = followed
  } else if (trackedJob.value) {
    trackedJob.value = candidates.find(job => job.job_id === trackedJob.value?.job_id) ?? trackedJob.value
  } else {
    trackedJob.value = candidates.find(job => ['queued', 'running', 'pausing', 'cancelling'].includes(job.state)) ?? candidates[0] ?? null
  }
}, { immediate: true })

onMounted(() => void refreshJobs())
</script>

<template>
  <div class="rootfs-apply-page">
    <RootfsApplyStep
      :open="true"
      :job-id="trackedJob?.job_id"
      :plan-id="trackedJob?.rootfs_plan?.plan_id ?? trackedJob?.plan_id"
      follow-only
      @close="navigate('overview')"
      @check-again="openRootfsView()"
    />
  </div>
</template>

<style scoped>
.rootfs-apply-page { height: 100%; overflow: auto; }
</style>
