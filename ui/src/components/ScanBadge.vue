<script setup lang="ts">
// The scan status every page shows in its page bar's meta (PageStatus): when the
// report's scan finished and how it read. An old or not fully read report warns;
// the reading sets the tone otherwise. Details and the page's notes open with it.
import { computed } from 'vue'
import { code, formatRelative, t } from '@/i18n'
import PageStatus from './PageStatus.vue'
import { consistencyTone, type ConsistencyTone } from './consistencyTone'
import { visibleConsistency, type DisplayReport } from './reportCompleteness'

const props = defineProps<{
  report: DisplayReport & { scan_finished: string; stale?: boolean }
  /** A reading the page settled on instead of the report's own. */
  consistency?: string
  details?: string
  /** What the report covers ("Whole volume", "Shared folder photos"), shown first when given. */
  scope?: string
  /** The page's secondary notes, which open with the status. */
  notes?: readonly string[]
}>()

const reading = computed(() => props.consistency ?? visibleConsistency(props.report) ?? 'partial')
const tone = computed<ConsistencyTone>(() => (props.report.stale ? 'warning' : consistencyTone(reading.value)))
const text = computed(() => {
  const parts = { time: formatRelative(props.report.scan_finished), consistency: code('consistency', reading.value) }
  return props.scope ? t('report.chip.scoped', { scope: props.scope, ...parts }) : t('overview.chip.scanned', parts)
})
</script>

<template>
  <PageStatus :label="text" :tone="tone" :details="details" :notes="notes" />
</template>
