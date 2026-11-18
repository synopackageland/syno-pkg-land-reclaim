// Dev-only responses for the apply, progress, result and History fixture page.
import { ref } from 'vue'
import { fixtureBackend, recordedFixtureBackend } from '@/api/fixtures'
import { withPlanningFixtureJobs } from '@/api/planningFixtureJobs'
import type { Backend } from '@/api/methods'
import type { Method } from '@/api/methods'
import historyRecorded from '../../../fixtures/rootfs.history/recorded.json'
import historyEmpty from '../../../fixtures/rootfs.history/empty.json'
import type { RootfsApplySuccessDocument } from '@/types'
import type { RootfsApplyResult, RootfsHistoryRecord as ApplyHistoryRecord } from './rootfsTypes'

export const ROOTFS_FIXTURE_SCENARIOS = [
  'moves', 'tools', 'cancellable', 'complete', 'partial', 'paused', 'failed',
  'interrupted', 'not-started', 'normal-again', 'still-short', 'history-normal', 'history-abnormal', 'history-empty',
] as const
export type RootfsFixtureScenario = typeof ROOTFS_FIXTURE_SCENARIOS[number]

const bootScenario = new URLSearchParams(window.location.search).get('scenario')
const isScenario = (value: string | null): value is RootfsFixtureScenario =>
  value != null && ROOTFS_FIXTURE_SCENARIOS.includes(value as RootfsFixtureScenario)

export const rootfsFixtureScenario = ref<RootfsFixtureScenario>(
  isScenario(bootScenario) ? bootScenario : 'moves',
)

let started = false
let cancelled = false

export function selectRootfsFixtureScenario(scenario: RootfsFixtureScenario) {
  rootfsFixtureScenario.value = scenario
  started = false
  cancelled = false
  backgroundBackend = withPlanningFixtureJobs(recordedRootfsApplyBackend as Backend)
}

function baseBackend() {
  const scenario = rootfsFixtureScenario.value === 'history-empty'
    ? 'no_report'
    : rootfsFixtureScenario.value === 'history-abnormal'
      ? 'sysvol_getting_full'
      : 'normal'
  return fixtureBackend({ scenario })
}

const JOB_ID = 'rootfs-job-fixture-0001'
const PLAN_ID = 'rootfs-plan-fixture-0001'
const TOTAL = 115_343_360

function completion(): 'complete' | 'partial' | 'paused' | 'failed' {
  if (cancelled || rootfsFixtureScenario.value === 'paused') return 'paused'
  if (rootfsFixtureScenario.value === 'partial') return 'partial'
  if (rootfsFixtureScenario.value === 'failed') return 'failed'
  return 'complete'
}

function jobProgress() {
  const phase = rootfsFixtureScenario.value === 'tools' ? 'tools' : 'moves'
  return phase === 'tools'
    ? { phase, items_done: 3, items_total: 3, bytes_done: 78_000_000, bytes_total: TOTAL, tool: 'rotate_logs' }
    : { phase, items_done: 2, items_total: 3, bytes_done: 78_000_000, bytes_total: TOTAL, tool: null }
}

function availableAfter() {
  if (rootfsFixtureScenario.value === 'normal-again') return 2_800_000_000
  if (rootfsFixtureScenario.value === 'still-short') return 390_000_000
  if (rootfsFixtureScenario.value === 'partial') return 700_000_000
  if (rootfsFixtureScenario.value === 'paused') return 600_000_000
  return 1_130_000_000
}

function applyResult(): RootfsApplyResult {
  const done = completion()
  const skipped: NonNullable<RootfsApplySuccessDocument['skipped']> = done === 'partial'
    ? [
        { path: '/var/log/upd@te/update.log', reason: 'changed' },
        { path: '/var/log/upd@te/old.log', reason: 'changed' },
        { path: '/var/log/auth.log', reason: 'held_open' },
      ]
    : []
  const failed = done === 'failed' || done === 'partial'
  return {
    schema_version: 1,
    command: 'rootfs-apply',
    tool_version: 'fixture',
    generated_at: '2026-09-30T14:15:00Z',
    completion: done,
    job_id: JOB_ID,
    errors: [],
    plan_id: PLAN_ID,
    plan_file: { path: `/var/lib/synoreclaim/rootfs-plans/${PLAN_ID}.json` },
    move_dir: { path: '/volume1/backup/SynoReclaim-system-20260930-1415' },
    manifest: { path: '/volume1/backup/SynoReclaim-system-20260930-1415/manifest.jsonl' },
    moved: { files: done === 'paused' ? 1 : 3, size_bytes: done === 'paused' ? 80_000_000 : TOTAL, removed_dirs: 0 },
    root: {
      available_before_bytes: 930_000_000,
      available_after_bytes: availableAfter(),
      freed_bytes: rootfsFixtureScenario.value === 'failed' ? 0 : 200_000_000,
    },
    skipped,
    tools: [
      { tool: 'rotate_logs', argv: ['synocleaner', '--rotate-log'], outcome: 'succeeded' },
      { tool: 'delete_journal', argv: ['synocleaner', '--delete-journal'], outcome: failed ? 'failed' : 'succeeded', ...(failed ? { exit_code: 1 } : {}) },
    ],
    notice: { code: 'no_restore', message: 'No restore is available.' },
    tools_notice: null,
    warnings: [],
  }
}

function reportScenario() {
  if (rootfsFixtureScenario.value === 'normal-again') return 'normal'
  if (rootfsFixtureScenario.value === 'still-short') return 'sysvol_full'
  return 'sysvol_getting_full'
}

function history() {
  return rootfsFixtureScenario.value === 'history-empty'
    ? historyEmpty as { applies: ApplyHistoryRecord[] }
    : historyRecorded as { applies: ApplyHistoryRecord[] }
}

/** Runtime also receives the U4 methods absent from main's generated Method union. */
let backgroundBackend = withPlanningFixtureJobs(recordedRootfsApplyBackend as Backend)
export async function rootfsApplyFixtureBackend(method: string, params: Record<string, unknown>) {
  return backgroundBackend(method as Method, params as never)
}
async function recordedRootfsApplyBackend(method: string, params: Record<string, unknown>) {
  if (method === 'jobs.list') {
    if (rootfsFixtureScenario.value.startsWith('history-')) {
      return baseBackend()(method as Method, params as never)
    }
    if (!started && !['interrupted', 'not-started'].includes(rootfsFixtureScenario.value)) return { jobs: [] }
    const state = rootfsFixtureScenario.value === 'interrupted' ? 'interrupted'
      : rootfsFixtureScenario.value === 'not-started' ? 'cancelled'
      : cancelled || rootfsFixtureScenario.value === 'paused'
      ? 'paused'
      : ['moves', 'tools', 'cancellable'].includes(rootfsFixtureScenario.value)
        ? 'running'
        : 'completed'
    return { jobs: [{ job_id: JOB_ID, kind: 'rootfs-apply', plan_id: PLAN_ID, rootfs_plan: { plan_id: PLAN_ID, move_dir: { path: '/volume1/backup/SynoReclaim-system-20260930-1415' } }, reason: rootfsFixtureScenario.value === 'not-started' ? 'not_started' : null, state, rootfs: rootfsFixtureScenario.value === 'not-started' ? undefined : jobProgress() }] }
  }
  if (method === 'rootfs.apply') {
    started = true
    cancelled = false
    return { job_id: JOB_ID, run: [] }
  }
  if (method === 'job.cancel') {
    cancelled = true
    return { outcome: 'requested' }
  }
  if (method === 'job.result') {
    if (['interrupted', 'not-started'].includes(rootfsFixtureScenario.value)) throw new Error('No final result')
    return applyResult()
  }
  if (method === 'rootfs.report') {
    return recordedFixtureBackend({ scenario: reportScenario() })('rootfs.report', {})
  }
  if (method === 'rootfs.history') return history()
  return baseBackend()(method as Method, params as never)
}

export function fixtureResult() {
  return applyResult()
}
