import { describe, expect, it } from 'vitest'
import {
  formatHistoryDateTime,
  hasFailedTools,
  isPausedResult,
  isRootfsApplyJob,
  levelAfterApply,
  progressRatio,
  rootfsOutcomeKey,
  rootfsSkipKey,
  rootfsToolKey,
  skippedByReason,
} from './rootfsApply'
import type { RootfsReportDocument } from '@/types'
import type { RootfsApplyResult } from './rootfsTypes'

const result = (changes: Partial<RootfsApplyResult> = {}): RootfsApplyResult => ({
  schema_version: 1,
  command: 'rootfs-apply',
  tool_version: 'test',
  generated_at: '2026-09-30T14:15:00Z',
  completion: 'complete',
  job_id: null,
  errors: [],
  warnings: [],
  plan_id: 'rootfs-abc',
  plan_file: { path: '/var/lib/synoreclaim/rootfs-plans/rootfs-abc.json' },
  move_dir: { path: '/volume1/backup/SynoReclaim-system-20260930-1415' },
  manifest: { path: '/volume1/backup/SynoReclaim-system-20260930-1415/manifest.jsonl' },
  moved: { files: 3, size_bytes: 115_343_360, removed_dirs: 0 },
  root: { available_before_bytes: 930_000_000, available_after_bytes: 1_130_000_000, freed_bytes: 200_000_000 },
  skipped: [],
  tools: [],
  notice: { code: 'no_restore', message: 'No restore is available.' },
  tools_notice: null,
  ...changes,
})

describe('rootfs apply progress and results', () => {
  it('formats compact localized history timestamps with date and time', () => {
    for (const lang of ['zh-TW', 'de'] as const) {
      const displayed = formatHistoryDateTime('2026-09-30T05:34:00Z', lang)
      expect(displayed).toContain('2026')
      expect(displayed).toContain('34')
      expect(displayed.length).toBeLessThan(20)
    }
  })

  it('recognizes only rootfs apply jobs', () => {
    expect(isRootfsApplyJob({ job_id: 'job-1', kind: 'rootfs-apply', state: 'running' })).toBe(true)
    expect(isRootfsApplyJob({ job_id: 'job-1', kind: 'scan', state: 'running' })).toBe(false)
  })

  it('calculates move progress by items and tool progress by bytes', () => {
    expect(progressRatio({ phase: 'moves', items_done: 2, items_total: 4, bytes_done: 90, bytes_total: 100, tool: null })).toBe(0.5)
    expect(progressRatio({ phase: 'tools', items_done: 2, items_total: 4, bytes_done: 75, bytes_total: 100, tool: 'rotate_logs' })).toBe(0.75)
    expect(progressRatio({ phase: 'tools', items_done: 0, items_total: 0, bytes_done: 0, bytes_total: 0, tool: null })).toBeNull()
  })

  it('groups skipped files by reason without losing distinct reasons', () => {
    expect(skippedByReason(result({ skipped: [
      { path: '/upd@te/a', reason: 'changed' },
      { path: '/upd@te/b', reason: 'changed' },
      { path: '/var/log/a.gz', reason: 'held_open' },
    ] }))).toEqual([
      { reason: 'changed', count: 2, paths: ['/upd@te/a', '/upd@te/b'] },
      { reason: 'held_open', count: 1, paths: ['/var/log/a.gz'] },
    ])
  })

  it('maps unknown server codes to translated fallbacks', () => {
    expect(rootfsToolKey('rotate_logs')).toBe('rootfsApply.tool.rotate_logs')
    expect(rootfsToolKey('new_tool')).toBe('rootfsApply.tool.unknown')
    expect(rootfsOutcomeKey('new_state')).toBe('rootfsApply.outcome.unknown')
    expect(rootfsSkipKey('new_reason')).toBe('rootfsApply.skip.unknown')
  })

  it('distinguishes paused results and failed DSM tools', () => {
    expect(isPausedResult(result({ completion: 'paused' }))).toBe(true)
    expect(hasFailedTools(result({ tools: [{ tool: 'delete_journal', argv: [], outcome: 'failed' }] }))).toBe(true)
    expect(hasFailedTools(result({ tools: [{ tool: 'rotate_logs', argv: [], outcome: 'succeeded' }] }))).toBe(false)
  })

  it('uses the same system-partition thresholds when a report omits level', () => {
    const report = (available_bytes: number) => ({
      filesystem: { total_bytes: 8_589_934_592, available_bytes },
    }) as unknown as RootfsReportDocument
    expect(levelAfterApply(report(900 * 1024 * 1024))).toBe('getting_full')
    expect(levelAfterApply(report(900 * 1024 * 1024 + 1))).toBe('ok')
    expect(levelAfterApply(report(2_800_000_000))).toBe('ok')
    expect(levelAfterApply(report(400_000_000))).toBe('full')
    expect(levelAfterApply(null)).toBeNull()
  })
})
