<script setup lang="ts">
// The SDK's shell with the app's pages, job strips and New scan.
import { computed, defineAsyncComponent, h, inject } from 'vue'
import { Button, LoadingArea, Surface } from '@synopackageland/ui'
import { activePage } from '@synopackageland/sdk'
import { AppShell, type AppShellPage } from '@synopackageland/sdk/vue'
import { Plus } from '@lucide/vue'
import { locale, t } from '@/i18n'
import { JobStrip, NewScanDialog, pages as registry } from '@/pages/registry'
import { nav, navigate, openNewScan } from '@/stores/nav'
import { selectedVolume } from '@/stores/volumes'
import { jobs, runningJobs } from '@/stores/jobs'
import { isActiveJob, isHaltedJob } from '@/stores/jobStates'
import { isVisiblePausedJob } from '@/components/scanProgress'
import UsageBaselineHint from '@/usage/UsageBaselineHint.vue'
import UsageChangeBanner from '@/usage/UsageChangeBanner.vue'
import { usageAlertShown, usageBaselineMissing } from '@/usage/useUsageChange'
import { SHELL_CONTEXT } from '@/context'

// While the first reads load, the chrome stands and each page's place says so.
const props = defineProps<{ loading?: boolean }>()
const ctx = inject(SHELL_CONTEXT, undefined)
const LoadingPage = () => h(LoadingArea, { text: t('common.loading') })

const showScenarios = import.meta.env.DEV && !window.SYNO
const DevScenarioSelect = import.meta.env.DEV
  ? defineAsyncComponent(() => import('./DevScenarioSelect.vue'))
  : null

// The page that follows a job shows its progress, so its row would repeat it.
const visibleJobs = computed(() => jobs.list.filter(isActiveJob).filter(job => !(
  (nav.page === 'scanning' && job.kind === 'scan')
  || (nav.page === 'scanning' && job.kind === 'rootfs-report' && job.job_id === nav.followedJobId)
  || (nav.page === 'cleanupApply' && job.kind === 'cleanup-apply' && job.job_id === nav.followedJobId)
  || (nav.page === 'rootfsApply' && job.kind === 'rootfs-apply' && job.job_id === nav.followedJobId)
)))

// The Scanning page's title follows its scan: a paused or interrupted scan is
// not Scanning, as its ring already says.
const scanningTitleKey = computed(() => {
  const scans = jobs.list.filter(job => job.kind === 'scan')
  const scan = scans.find(job => job.job_id === nav.followedJobId) ?? scans.find(isActiveJob) ?? scans.find(job => isVisiblePausedJob(job))
  return scan && isHaltedJob(scan) ? `scan.status.${scan.state}` : null
})

// A volume without snapshots drops the page, so a stale choice falls back to
// the first listed page; pages without an icon are reached only by following a job.
const pages = computed<AppShellPage[]>(() => registry
  .filter(p => p.key !== 'snapshots' || selectedVolume.value?.capabilities.snapshots !== false)
  .map(p => ({
    id: p.key,
    title: t((p.key === 'scanning' && scanningTitleKey.value) || p.titleKey),
    icon: p.icon,
    hidden: !p.icon,
    component: props.loading ? LoadingPage : p.component,
    fill: props.loading || p.fill,
    placement: p.placement,
    badge: p.key === 'history' && runningJobs.value.length ? runningJobs.value.length : undefined,
  })))
const page = computed(() => activePage(nav.page, pages.value))
</script>

<template>
  <AppShell :model-value="page" :pages="pages" :ctx="ctx" :locale="locale" page-width="fill" sidebar-storage-key="synoreclaim" @update:model-value="navigate">
    <template #notices="{ page: current }">
      <!-- One aligned block: a row per running job, then Overview's no-baseline hint;
           Explore keeps that hint behind its info button, so its list starts higher. -->
      <Surface v-if="visibleJobs.length || (current === 'overview' && usageBaselineMissing)" class="status-list" role="list" padding="none" :aria-label="t('shell.status')">
        <JobStrip v-for="job in visibleJobs" :key="job.job_id" :job="job" />
        <UsageBaselineHint v-if="current === 'overview' && usageBaselineMissing" />
      </Surface>
      <UsageChangeBanner v-if="(current === 'overview' || current === 'explore') && usageAlertShown" />
    </template>
    <!-- Settings starts no scan. -->
    <template #actions="{ page: current }">
      <template v-if="current !== 'settings' && !loading">
        <component :is="DevScenarioSelect" v-if="showScenarios && DevScenarioSelect" />
        <Button variant="primary" @click="openNewScan"><Plus class="size-4" />{{ t('shell.newScan') }}</Button>
      </template>
    </template>
  </AppShell>
  <NewScanDialog v-model:open="nav.newScanOpen" />
</template>

<style scoped>
/* ListItem rows carry their own vertical rhythm and dividers; the block gives them an inset. */
.status-list { padding-inline: var(--ui-space-4); }
</style>
