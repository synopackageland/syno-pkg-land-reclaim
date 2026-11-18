import { changesOnlyInSystemAreas, type ConsistentReport } from './consistency'

const shares = [{ subvolume_id: 262 }]

const report = (reasons: unknown[], consistency: ConsistentReport['consistency'] = 'changed-during-scan'): ConsistentReport => ({
  consistency,
  change_reasons: reasons as ConsistentReport['change_reasons'],
})

const generation = (subvolume_id: number) => ({
  kind: 'subvolume-generation-changed',
  subvolume_id,
  selected: false,
  generation_before: 1,
  generation_after: 2,
})

describe('changes only in system areas', () => {
  it('is true when every changed subvolume is not a shared folder', () => {
    expect(changesOnlyInSystemAreas(report([generation(256)]), shares)).toBe(true)
  })

  it('is false when a shared folder subvolume changed', () => {
    expect(changesOnlyInSystemAreas(report([generation(256), generation(262)]), shares)).toBe(false)
  })

  it('is false when an appeared subvolume is a snapshot of a shared folder', () => {
    const appeared = { kind: 'subvolume-appeared', subvolume_id: 300, generation: 1, snapshot_of: 262 }
    expect(changesOnlyInSystemAreas(report([appeared]), shares)).toBe(false)
  })

  it('is false when a reason does not name a subvolume (a balance)', () => {
    expect(changesOnlyInSystemAreas(report([{ kind: 'maintenance-running', operation: 'balance' }]), shares)).toBe(false)
  })

  it('is false when a changed subvolume vanished, so its share membership cannot be read', () => {
    const vanished = { kind: 'subvolume-vanished', subvolume_id: 256, generation_before: 1 }
    expect(changesOnlyInSystemAreas(report([vanished]), shares)).toBe(false)
  })

  it('is false when a relocation root was left behind (a balance, not DSM writes)', () => {
    expect(changesOnlyInSystemAreas(report([{ kind: 'relocation-tree', subvolume_id: 256 }]), shares)).toBe(false)
  })

  it('is false without change reasons or without changed-during-scan', () => {
    expect(changesOnlyInSystemAreas(report([]), shares)).toBe(false)
    expect(changesOnlyInSystemAreas(report([generation(256)], 'observed-stable'), shares)).toBe(false)
  })
})
