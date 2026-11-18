<script setup="ts" lang="ts">
import { ref, watch } from 'vue'
import { useExecutorPreload } from '@synopackageland/sdk/vue'
import { jobs, runningJobs } from '@/stores/jobs'
import { nav, navigate } from '@/stores/nav'
import ReviewDialog from './ReviewDialog.vue'

// Its dialog would leave the pages ExecutorApproval preloads hidden; it opens once they show.
const preloading = useExecutorPreload()
const trackedJobId = ref(jobs.list.find(job => job.job_id === nav.followedJobId && job.kind === 'cleanup-apply')?.job_id ?? null)
watch(() => nav.followedJobId, id => {
  if (jobs.list.some(job => job.job_id === id && job.kind === 'cleanup-apply')) trackedJobId.value = id
})
watch(() => jobs.list, () => {
  trackedJobId.value ??= runningJobs.value.find(job => job.kind === 'cleanup-apply')?.job_id ?? null
  if (trackedJobId.value) nav.followedJobId = trackedJobId.value
}, { immediate: true })
</script>

<template>
  <ReviewDialog v-if="trackedJobId" :open="!preloading" :job-id="trackedJobId" @update:open="navigate('overview')" />
</template>
