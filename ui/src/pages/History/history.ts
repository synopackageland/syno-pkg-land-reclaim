// Rows for the History page: finished scans from
// history.list with the still-open jobs of jobs.list merged in, newest first.
import { reportCompleteness, type Attribution } from '@/components/reportCompleteness'
import { isActiveJob, isHaltedJob } from '@/stores/jobStates'
import type { Params } from '@/i18n'
import type { HistoryDocument, Job, Path } from '@/types'

export type BadgeTone = 'neutral' | 'info' | 'success' | 'warning' | 'danger'

export type ScanRow = {
  id: string
  kind: Job['kind']
  reason: string | null
  missingResult: boolean
  started: string | null
  /** When a recorded scan finished; null while a job is still open. */
  finished: string | null
  targets: Path[]
  volume: boolean
  profile: string | null
  elapsedMs: number | null
  entries: number | null
  peakRssBytes: number | null
  outcome: string
  attribution?: Attribution
  live: boolean
  /** A published report this row can open. */
  hasReport: boolean
}

const TONES: Record<string, BadgeTone> = {
  completed: 'success',
  partial: 'warning',
  failed: 'danger',
  cancelled: 'neutral',
  superseded: 'neutral',
  paused: 'info',
  interrupted: 'info',
  queued: 'info',
  running: 'info',
  pausing: 'info',
  cancelling: 'info',
}

export const outcomeTone = (outcome: string): BadgeTone => TONES[outcome] ?? 'neutral'

function startedOf(row: ScanRow) {
  const at = row.started ? Date.parse(row.started) : NaN
  return Number.isNaN(at) ? 0 : at
}

export function historyRows(history: HistoryDocument, jobs: Job[]): ScanRow[] {
  const live: ScanRow[] = jobs.filter(job => isActiveJob(job) || isHaltedJob(job) || (['failed', 'cancelled'].includes(job.state) && (job.kind !== 'scan' || job.result == null))).map(job => ({
    id: job.job_id,
    kind: job.kind,
    reason: job.reason,
    missingResult: ['interrupted', 'failed', 'cancelled'].includes(job.state) && job.result == null,
    started: job.created,
    finished: null,
    targets: job.targets,
    volume: job.scope === 'volume',
    profile: job.kind === 'scan' ? job.profile : null,
    elapsedMs: job.progress ? job.progress.elapsed_s * 1000 : null,
    entries: job.kind === 'scan' ? job.progress?.entries ?? null : null,
    peakRssBytes: job.progress?.rss_bytes ?? null,
    outcome: job.state,
    live: true,
    hasReport: job.kind === 'scan' && job.result != null && job.result.reports.length > 0,
  }))
  const liveIds = new Set(live.map(row => row.id))
  const finished: ScanRow[] = history.scans
    .filter(scan => !liveIds.has(scan.job_id))
    .map(scan => ({
      id: scan.job_id,
      kind: 'scan',
      reason: null,
      missingResult: false,
      started: scan.started,
      finished: scan.finished,
      targets: scan.targets,
      volume: scan.volume,
      profile: scan.profile ?? null,
      elapsedMs: scan.elapsed_ms,
      entries: scan.entries,
      peakRssBytes: scan.peak_rss_bytes ?? null,
      outcome: scan.outcome,
      attribution: scan.attribution,
      live: false,
      hasReport: scan.report != null,
    }))
  return [...live, ...finished].sort((a, b) => startedOf(b) - startedOf(a))
}

/** Whether a scan's target set covers a volume mount or something below it. */
export function coversVolume(row: Pick<ScanRow, 'targets'>, mount: string) {
  return row.targets.some(target => target.path === mount || target.path.startsWith(`${mount}/`))
}

/** One used-space point per scan time of the volume, oldest first; scans that finished together are one measurement. */
export function trendPoints(history: HistoryDocument, mount: string) {
  const byTime = new Map<string, number>()
  for (const scan of history.scans) {
    if (scan.df_used_bytes != null && coversVolume(scan, mount) && !byTime.has(scan.finished)) byTime.set(scan.finished, scan.df_used_bytes)
  }
  return [...byTime].map(([at, value]) => ({ at, value })).sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
}

const OUTCOMES = new Set([
  'completed',
  'partial',
  'cancelled',
  'failed',
  'paused',
  'superseded',
  'running',
  'pausing',
  'cancelling',
  'queued',
  'interrupted',
])

export const isKnownOutcome = (outcome: string) => OUTCOMES.has(outcome)

const DAY_MS = 86_400_000
const DATE: Intl.DateTimeFormatOptions = { month: '2-digit', day: '2-digit' }
const MINUTE: Intl.DateTimeFormatOptions = { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }
const SECOND: Intl.DateTimeFormatOptions = { ...MINUTE, second: '2-digit' }

/**
 * `count` x-axis labels spread evenly from `first` over `spanMs`: dates, or
 * times of day for a span under a day, with more precision wherever two
 * neighbouring labels would read the same.
 */
export function trendAxisLabels(first: number, spanMs: number, count: number, { locale, timeZone }: { locale: string; timeZone?: string }) {
  const times = Array.from({ length: count }, (_, index) => first + (spanMs * index) / Math.max(1, count - 1))
  const formats = spanMs < DAY_MS ? [MINUTE, SECOND] : [DATE, { ...DATE, ...MINUTE }]
  let labels: string[] = []
  for (const options of formats) {
    const format = new Intl.DateTimeFormat(locale, { ...options, timeZone })
    labels = times.map(time => format.format(time))
    if (labels.every((label, index) => index === 0 || label !== labels[index - 1])) break
  }
  return labels
}

const SPAN_UNITS = [
  { unit: 'minutes', ms: 60_000, below: 60 },
  { unit: 'hours', ms: 3_600_000, below: 24 },
  { unit: 'days', ms: DAY_MS, below: Infinity },
] as const

/** "{first} → {last} over {span}", the span in the largest unit it rounds to at least one of. */
export function trendSubtitle(first: string, last: string, spanMs: number, t: (key: string, params?: Params) => string) {
  const { unit, n } = SPAN_UNITS
    .map(({ unit, ms, below }) => ({ unit, n: Math.max(1, Math.round(spanMs / ms)), below }))
    .find(({ n, below }) => n < below)!
  return t('history.trend.subtitle', { first, last, span: t(`history.trend.span.${unit}`, { count: n }) })
}

/** Only the visible badge changes; outcome remains the saved value. */
export function visibleOutcome(row: ScanRow) {
  if (row.kind === 'scan' && row.outcome === 'completed' && row.attribution?.approximation) return 'approximate'
  if (row.kind !== 'scan' || row.outcome !== 'partial') return row.outcome
  const { warn, churnEntries } = reportCompleteness({ consistency: 'partial', attribution: row.attribution }, row.outcome)
  if (warn) return 'partial'
  return churnEntries ? 'changed-during-scan' : 'small_unknown'
}
