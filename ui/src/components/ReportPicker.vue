<script setup lang="ts">
// The report a page answers from: the
// newest report of each path, or one saved report the user picks. Each choice
// names what the scan read and when, so the scope on screen is never a guess.
import { computed } from 'vue'
import { Select } from '@synopackageland/ui'
import { formatRelative, t } from '@/i18n'
import { NEWEST_REPORT, scopeText, type ScanRecord } from './reportChoice'

defineOptions({ name: 'ReportPicker' })

const props = defineProps<{
  /** The chosen report's job ID, or null for the newest of each path. */
  modelValue: string | null
  /** The saved reports covering what the page shows, newest first. */
  reports: ScanRecord[]
  mount: string
  /** Whether "newest" mixes newer share reports into an older volume report here. */
  mixed?: boolean
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string | null] }>()

const options = computed(() => [
  { value: NEWEST_REPORT, label: t(props.mixed ? 'report.choice.newestEach' : 'report.choice.newest') },
  ...props.reports.map(scan => ({ value: scan.job_id, label: reportLabel(scan) })),
])

function reportLabel(scan: ScanRecord) {
  return t('report.choice.saved', { scope: scopeText(scan, props.mount), time: formatRelative(scan.finished) })
}

function choose(value: string) {
  emit('update:modelValue', value === NEWEST_REPORT ? null : value)
}
</script>

<template>
  <Select
    class="report-picker"
    :model-value="modelValue ?? NEWEST_REPORT"
    :options="options"
    :aria-label="t('report.choice.label')"
    @update:model-value="choose(String($event))"
  />
</template>

<style scoped>
.report-picker {
  min-width: 0;
  max-width: 260px;
}
</style>
