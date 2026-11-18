<script setup lang="ts">
import { computed, ref } from 'vue'
import { Button, Callout } from '@synopackageland/ui'
import { api } from '@/api'
import { formatBytes, formatCount, formatDuration, t } from '@/i18n'
import { authorizeRootWork } from '@/stores/authorization'
import { apiErrorText } from '@/components/states/apiErrorText'
import { jobs, pollNow } from '@/stores/jobs'
import { isActiveJob } from '@/stores/jobStates'
import { openJob } from '@/stores/nav'
import type { OverviewDocument, Path } from '@/types'

const props = defineProps<{ result: OverviewDocument['results'][number]; mount: Path }>()
const state = computed(() => props.result.attribution)
const busy = ref(false)
const error = ref('')
const running = computed(() => jobs.list.some(isActiveJob))
async function start() {
  if (busy.value || running.value || !state.value?.deferred_bytes) return
  busy.value = true
  error.value = ''
  try {
    if (!(await authorizeRootWork())) return
    const response = await api('scan.start', { targets: [props.mount], mode: 'gentle', attribution_report: props.result.report.job_id })
    if (response.outcome === 'scan_in_progress') { error.value = t('state.busy.title'); return }
    if (response.job_id) openJob({ job_id: response.job_id, kind: 'scan' })
    pollNow()
  } catch (e) { error.value = apiErrorText(e) }
  finally { busy.value = false }
}
</script>

<template>
  <Callout v-if="state?.deferred_bytes" :dismissible="false" tone="info" :title="t('overview.deep.title')" data-testid="deep-attribution">
    {{ t('overview.deep.body', { bytes: formatBytes(state.deferred_bytes) }) }}
    <template #actions><Button :disabled="busy || running" :loading="busy" @click="start">{{ t('overview.deep.start') }}</Button></template>
    <template #details>{{ t('overview.deep.budget', { reads: formatCount(state.read_budget?.selected_reads ?? null) }) }}</template>
  </Callout>
  <Callout v-else-if="state?.analysis" tone="info" :title="t('overview.deep.done', { duration: formatDuration(state.analysis.elapsed_ms / 1000) })" data-testid="deep-attribution-completed">
    {{ t('overview.deep.result', { before: formatBytes(state.analysis.unknown_before), after: formatBytes(state.analysis.unknown_after) }) }}
  </Callout>
  <Callout v-if="error" tone="danger" :title="error" />
</template>
