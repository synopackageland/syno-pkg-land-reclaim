// Pure df-usage comparison and alert transitions.
import type { HistoryDocument, Volume } from '@/types'

export const SAMPLE_INTERVAL = 15 * 60 * 1000
/** How long the app, once open, leaves the NAS to the first page's reads before it takes its first sample. */
export const FIRST_SAMPLE_DELAY = 3000
export const WEEK = 7 * 24 * 60 * 60 * 1000
export const threshold = (total: number) => Math.max(total * 0.05, 20_000_000_000)

export interface Baseline { id: string; fsid: string; total: number; used: number }
export interface AlertRecord {
  baselineId: string
  crossed: boolean
  sampledAt: number
  visible: boolean
  lastAlert: number | null
  snoozedUntil: number | null
  dismissedUsed: number | null
}

const bytes = (value: number | null | undefined): value is number =>
  value != null && Number.isSafeInteger(value) && value >= 0

/** Select the newest complete scan first; incompatibility must not revive an older baseline. */
export function baselineFor(volume: Volume, scans: HistoryDocument['scans']): Baseline | null {
  const latest = scans.filter(scan => scan.volume
    && scan.targets.some(target => target.path === volume.mount.path && target.path_hex === volume.mount.path_hex)
    && (scan.outcome === 'completed' || (scan.outcome === 'superseded' && scan.original_outcome === 'completed')))
    .sort((a, b) => Date.parse(b.finished) - Date.parse(a.finished))[0]
  if (!latest || !volume.fsid || latest.fsid !== volume.fsid
    || !bytes(volume.used_bytes) || !bytes(volume.total_bytes)
    || !bytes(latest.df_used_bytes) || !bytes(latest.df_total_bytes)
    || latest.df_total_bytes === 0 || latest.df_total_bytes !== volume.total_bytes
    || latest.df_used_bytes > latest.df_total_bytes || volume.used_bytes > volume.total_bytes) return null
  return { id: `${latest.job_id}:${latest.finished}`, fsid: volume.fsid, total: latest.df_total_bytes, used: latest.df_used_bytes }
}

export function sample(baseline: Baseline, used: number, previous: AlertRecord | undefined, now: number): AlertRecord {
  const state: AlertRecord = previous?.baselineId === baseline.id ? { ...previous } : {
    baselineId: baseline.id, crossed: false, sampledAt: -1, visible: false,
    lastAlert: null, snoozedUntil: null, dismissedUsed: null,
  }
  if (now <= state.sampledAt) return state
  const crosses = bytes(used) && Math.abs(used - baseline.used) >= threshold(baseline.total)
    && (state.dismissedUsed == null || Math.abs(used - state.dismissedUsed) >= threshold(baseline.total))
  const confirmed = state.crossed && crosses
  state.crossed = crosses
  state.sampledAt = now
  if (!crosses) state.visible = false
  if (!state.visible && confirmed
    && (state.lastAlert == null || now - state.lastAlert >= WEEK)
    && (state.snoozedUntil == null || now >= state.snoozedUntil)) {
    state.visible = true
    state.lastAlert = now
    state.snoozedUntil = null
    state.dismissedUsed = null
  }
  return state
}

export const snooze = (state: AlertRecord, now: number): AlertRecord =>
  ({ ...state, visible: false, snoozedUntil: now + WEEK, dismissedUsed: null })
export const dismiss = (state: AlertRecord, used: number): AlertRecord =>
  ({ ...state, crossed: false, visible: false, snoozedUntil: null, dismissedUsed: used })
