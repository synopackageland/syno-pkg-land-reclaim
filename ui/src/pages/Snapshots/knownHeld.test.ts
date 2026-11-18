import type { TopDocument } from '@/types'
import { snapshotHeld } from './knownHeld'

type Result = TopDocument['results'][number]
type Entry = Result['entries'][number]

const entry = (path: string, snapshot: number, extra: Record<string, unknown> = {}): Entry => ({
  path, kind: 'dir', flags: [], metrics: { physical: snapshot * 3 }, blockers: [], nlink: 1, tree: 256, objectid: 1,
  service: null, cleanup_eligible: true,
  held_by: snapshot ? [{ class: 'reflink', bytes: 1 }, { class: 'snapshot', bytes: snapshot }] : [],
  where_to_fix: { token: 'cleanup', manage_in: null, manage_in_id: null, command: null },
  ...extra,
}) as unknown as Entry

const result = (entries: Entry[]) => ({ entries }) as Result

describe('snapshotHeld', () => {
  it('lists the rows snapshots also hold, largest first, with their total', () => {
    const held = snapshotHeld(result([entry('/v/a', 10), entry('/v/b', 0), entry('/v/c', 30)]), 200)
    expect(held.rows.map(row => [row.path, row.snapshotBytes])).toEqual([['/v/c', 30], ['/v/a', 10]])
    expect(held.total).toBe(40)
    expect(held.lowerBound).toBe(false)
  })

  // Explore draws the same rows; one included in another (`within`) would count twice.
  it('leaves out a row already included in another row', () => {
    const held = snapshotHeld(result([
      entry('/v/NetBackup', 20),
      entry('/v/NetBackup/x.hbk', 15, { kind: 'service', within: { path: '/v/NetBackup' } }),
    ]), 200)
    expect(held.rows.map(row => row.path)).toEqual(['/v/NetBackup'])
    expect(held.total).toBe(20)
  })

  it('names package data by its package, as Explore and Packages do', () => {
    const abb = entry('/volume1/ABB FS symbolic link test', 494_500_000_000, {
      kind: 'service',
      service: { id: 'active_backup_data', names: '@ActiveBackup', managed_by: 'x', manage_in: 'x', manage_in_id: 'x', confirmed: true, paths: [], path_count: 1 },
    })
    const [row] = snapshotHeld(result([abb]), 200).rows
    expect(row).toMatchObject({ name: 'Active Backup for Business (backup data)', folder: 'ABB FS symbolic link test', snapshotBytes: 494_500_000_000 })
  })

  it('is a lower bound when the answer reached its row limit', () => {
    expect(snapshotHeld(result([entry('/v/a', 1), entry('/v/b', 1)]), 2).lowerBound).toBe(true)
  })

  it('is empty without an answer', () => {
    expect(snapshotHeld(undefined, 200)).toEqual({ rows: [], total: 0, lowerBound: false })
  })
})
