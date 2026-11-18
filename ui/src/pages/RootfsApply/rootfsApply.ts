import type { RootfsReportDocument } from '@/types'
import type { RootfsApplyJob, RootfsApplyProgress, RootfsApplyResult } from './rootfsTypes'
import { systemPartitionLevel } from '@/pages/Overview/systemPartition'

const TOOL_CODES = new Set(['rotate_logs', 'delete_journal', 'logrotate', 'restart_syslog_ng', 'clean_downloaded_update', 'release_update_reserve', 'delete_log'])
const OUTCOME_CODES = new Set([
  'complete', 'completed', 'partial', 'paused', 'failed', 'succeeded', 'not_needed',
  'killed', 'timed_out', 'untrusted', 'not_started',
])
const SKIP_CODES = new Set([
  'missing', 'changed', 'path_changed', 'changed_during_copy', 'held_open', 'copy_mismatch',
  'copy_exists', 'no_room', 'not_empty', 'unlink_failed',
])
const CATEGORY_CODES = new Set([
  'update_leftover', 'log_archive', 'crash_dump', 'temp_debris', 'logs', 'log_center_db',
  'geoip_database', 'preference_image', 'samba_cache', 'tool_cache', 'backup_installer', 'package_data', 'user_tooling', 'dsm_system', 'other',
])

export function rootfsToolKey(tool: string) {
  return `rootfsApply.tool.${TOOL_CODES.has(tool) ? tool : 'unknown'}`
}

export function rootfsOutcomeKey(outcome: string) {
  return `rootfsApply.outcome.${OUTCOME_CODES.has(outcome) ? outcome : 'unknown'}`
}

export function rootfsSkipKey(reason: string) {
  return `rootfsApply.skip.${SKIP_CODES.has(reason) ? reason : 'unknown'}`
}

export function rootfsCategoryKey(category: string) {
  return `rootfsApply.category.${CATEGORY_CODES.has(category) ? category : 'other'}`
}

export function isRootfsApplyJob(value: unknown): value is RootfsApplyJob {
  if (!value || typeof value !== 'object') return false
  const job = value as Partial<RootfsApplyJob>
  return job.kind === 'rootfs-apply' && typeof job.job_id === 'string' && typeof job.state === 'string'
}

export function progressRatio(progress: RootfsApplyProgress | undefined) {
  if (!progress) return null
  const total = progress.phase === 'moves' ? progress.items_total : progress.bytes_total
  const done = progress.phase === 'moves' ? progress.items_done : progress.bytes_done
  if (!(total > 0)) return null
  return Math.max(0, Math.min(1, done / total))
}

export function formatHistoryDateTime(value: string, locale: string) {
  return new Intl.DateTimeFormat(locale, {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(new Date(value))
}

export function skippedByReason(result: RootfsApplyResult | null) {
  const groups = new Map<string, { reason: string; count: number; paths: string[] }>()
  for (const item of result?.skipped ?? []) {
    const group = groups.get(item.reason) ?? { reason: item.reason, count: 0, paths: [] }
    group.count++
    group.paths.push(item.path)
    groups.set(item.reason, group)
  }
  return [...groups.values()]
}

export function hasFailedTools(result: RootfsApplyResult | null) {
  return (result?.tools ?? []).some(tool => ['failed', 'killed', 'timed_out', 'untrusted', 'not_started'].includes(tool.outcome))
}

export function isPausedResult(result: RootfsApplyResult | null) {
  return result?.completion === 'paused'
}

export function levelAfterApply(report: RootfsReportDocument | null) {
  if (!report) return null
  if (report.filesystem.level) return report.filesystem.level
  const total = report.filesystem.total_bytes
  const available = report.filesystem.available_bytes
  if (!(total > 0)) return null
  return systemPartitionLevel({
    mount: '/',
    source: '/dev/md0',
    fstype: 'ext4',
    total_bytes: total,
    available_bytes: available,
    nearly_full: available < total * 0.05,
  })
}
