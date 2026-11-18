import { HALTED_STATES, isActiveJob } from '@/stores/jobStates'
import type { Job } from '@/types'

export const PHASES = ['discover', 'scan', 'resolve-refs', 'aggregate', 'publish'] as const

const SEVEN_DAYS = 7 * 24 * 60 * 60 * 1000

export function isVisiblePausedJob(job: Job, now = Date.now()) {
  return job.kind === 'scan' && HALTED_STATES.includes(job.state) && now - Date.parse(job.updated) <= SEVEN_DAYS
}

/** The scan that is running, or paused/interrupted within a week, on a volume (active first). */
export function scanJobOnVolume(jobs: Job[], mountPath: string, now = Date.now()) {
  const onVolume = jobs.filter(job => job.kind === 'scan' && job.filesystems.some(fs => fs.mount.path === mountPath))
  return onVolume.find(isActiveJob) ?? onVolume.find(job => isVisiblePausedJob(job, now))
}

function entryProgressRatio(entries: number | null | undefined, basisEntries: number | null | undefined) {
  if (entries == null || basisEntries == null || basisEntries <= 0) return null
  // Whole percents, so the ring, the strip bar and the Overview chip agree.
  return Math.min(0.99, Math.max(0, Math.round((entries / basisEntries) * 100) / 100))
}

/** Share of the last scan read so far (SPEC §6.2): metadata items when both runs have them, else entries. */
export function progressRatio(progress: Job['progress'] | undefined) {
  if (progress?.restart) return null
  if (progress?.phase === 'snapshots' || progress?.phase === 'publish') return null
  if (progress?.items != null && progress.basis?.items != null) return entryProgressRatio(progress.items, progress.basis.items)
  return entryProgressRatio(progress?.entries, progress?.basis?.entries)
}

export function progressPhaseIndex(phase: string | undefined) {
  const normalized = phase === 'snapshots' ? 'aggregate' : phase
  return PHASES.indexOf(normalized as (typeof PHASES)[number])
}

export function normalizedPhase(phase: string | undefined) {
  return phase === 'snapshots' ? 'aggregate' : phase ?? 'discover'
}

export function modeForProfile(profile: Job['profile']): 'fast' | 'gentle' {
  return profile === 'low' ? 'gentle' : 'fast'
}

type PathLike = { path: string; path_hex?: string }
type NamedShare = { path: PathLike; name: PathLike }

/**
 * Shorten a long path for a narrow card, keeping both ends so the name and the
 * leaf stay visible. The full value stays available to the caller.
 */
export function middleEllipsis(text: string, max = 48): string {
  if (text.length <= max) return text
  const keep = max - 1
  const head = Math.ceil(keep / 2)
  const tail = keep - head
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`
}

/**
 * What the scan is reading now: a known shared folder and the path below it,
 * else the elided path. The folder name is used only when the path is really
 * inside that share, so an unknown subvolume path is never renamed.
 */
export function currentLocation(
  current: PathLike | null | undefined,
  shares: readonly NamedShare[],
  max = 48,
): string | null {
  if (!current?.path) return null
  const full = current.path.replace(/\/+$/, '')
  for (const share of shares) {
    const root = share.path.path.replace(/\/+$/, '')
    if (!root) continue
    if (full === root) return share.name.path
    if (full.startsWith(`${root}/`)) {
      return `${share.name.path} › ${middleEllipsis(full.slice(root.length + 1), max)}`
    }
  }
  return middleEllipsis(full, max)
}

/** When the shown jobs.list answer was made at the server and arrived here (client ms). */
export interface PollTime {
  generatedAt: string
  receivedAt: number
}

/**
 * Seconds the scan has run at client time `now`: the polled elapsed_s, plus
 * how old that progress already was when the server answered, plus the time
 * since the answer arrived, so the count moves every second between polls and
 * each poll corrects it. A scan that is not running holds its polled value.
 */
export function liveElapsed(job: Pick<Job, 'state' | 'progress'>, poll: PollTime | null, now: number) {
  const progress = job.progress
  if (!progress) return undefined
  if (job.state !== 'running' || !poll) return progress.elapsed_s
  const age = Math.max(0, Date.parse(poll.generatedAt) - Date.parse(progress.updated)) || 0
  const since = Math.max(0, now - poll.receivedAt)
  return progress.elapsed_s + Math.floor((age + since) / 1000)
}
