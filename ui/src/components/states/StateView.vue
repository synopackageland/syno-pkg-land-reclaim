<script setup lang="ts">
// One view per global state. Blocking states are
// EmptyStates; partial, busy and error are Callouts over the page. An old
// report is the header badge's to say (ScanBadge).
import { computed, type Component } from 'vue'
import { Button, Callout, EmptyState } from '@synopackageland/ui'
import { HardDrive, PackageX, ScanSearch, ShieldAlert } from '@lucide/vue'
import { code, formatCount, formatDuration, joinParts, locale, t } from '@/i18n'
import { normalizedPhase, scanJobOnVolume } from '@/components/scanProgress'
import { isActiveJob } from '@/stores/jobStates'
import { jobs } from '@/stores/jobs'
import { navigate } from '@/stores/nav'
import { selectedVolume } from '@/stores/volumes'
import type { ApiError } from '@/api/errors'
import { apiErrorText } from './apiErrorText'
import type { StateKind } from './states'

const props = defineProps<{
  kind: StateKind
  /** CLI codes explaining the state, e.g. errors[].code or unknown reasons. */
  codes?: { set: string; value: string }[]
  /** A refused call explaining the state, shown before the codes. */
  error?: ApiError | null
  /** no_report: how long the last full scan took (scan_hints[].last_scan). */
  lastScanSeconds?: number | null
}>()

type View = { title: string; body?: string; icon?: Component; tone?: 'info' | 'warning' | 'danger' }

const VIEWS: Record<StateKind, () => View> = {
  not_admin: () => ({ title: t('state.notAdmin.title'), icon: ShieldAlert }),
  session_expired: () => ({ title: t('state.sessionExpired.title'), body: t('state.sessionExpired.body'), icon: ShieldAlert }),
  backend_unavailable: () => ({ title: t('state.backendUnavailable.title'), body: t('state.backendUnavailable.body'), icon: PackageX }),
  no_supported_volume: () => ({ title: t('state.noSupportedVolume.title'), body: t('state.noSupportedVolume.body'), icon: HardDrive }),
  no_report: () => ({
    title: t('state.noReport.title'),
    body: props.lastScanSeconds != null ? t('state.noReport.lastScan', { duration: formatDuration(props.lastScanSeconds) }) : t('state.noReport.body'),
    icon: ScanSearch,
  }),
  partial: () => ({ title: t('state.partial.title'), body: t('state.partial.body'), tone: 'warning' }),
  busy: () => ({ title: t('state.busy.title'), tone: 'warning' }),
  error: () => ({ title: t('state.error.title'), tone: 'danger' }),
}

// A volume that already has a scan job is being scanned, not "not scanned yet":
// show that scan and never offer to start another one.
const scanJob = computed(() =>
  props.kind === 'no_report' && selectedVolume.value ? scanJobOnVolume(jobs.list, selectedVolume.value.mount.path) : undefined,
)
const scanBody = computed(() => {
  const progress = scanJob.value?.progress
  return joinParts([t('scan.progress.items', { entries: formatCount(progress?.entries) }), progress?.phase ? t(`scan.phase.${normalizedPhase(progress.phase)}`) : ''], locale)
})

const view = computed(() => VIEWS[props.kind]())
const reasons = computed(() => [...(props.error ? [apiErrorText(props.error)] : []), ...(props.codes ?? []).map(c => code(c.set, c.value))])
</script>

<template>
  <EmptyState v-if="scanJob" :title="t(isActiveJob(scanJob) ? 'state.scanning.title' : 'state.scanning.paused')" :description="scanBody" :icon="ScanSearch">
    <template #actions><Button variant="primary" data-testid="view-scan" @click="navigate('scanning')">{{ t('newScan.busy.view') }}</Button></template>
  </EmptyState>
  <EmptyState v-else-if="!view.tone" :title="view.title" :description="view.body" :icon="view.icon">
    <template v-if="$slots.default" #actions><slot /></template>
  </EmptyState>
  <Callout v-else :dismissible="false" :tone="view.tone" :title="view.title">
    <p v-if="view.body">{{ view.body }}</p>
    <ul v-if="reasons.length">
      <li v-for="reason in reasons" :key="reason">{{ reason }}</li>
    </ul>
    <template v-if="$slots.default" #actions><slot /></template>
  </Callout>
</template>
