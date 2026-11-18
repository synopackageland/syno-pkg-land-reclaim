import { entryNotes, protectedKind } from './entryNotes'
import type { ExploreEntry } from './treemap'

function entry(change: Partial<Pick<ExploreEntry, 'blockers' | 'nlink' | 'flags'>> & { unknown?: number; pinned?: number | null } = {}) {
  return {
    path: '/volume1/share04/file.bin',
    kind: 'file',
    flags: change.flags ?? [],
    blockers: change.blockers ?? [],
    nlink: change.nlink ?? 1,
    metrics: { logical: 10, physical: 10, shared: 0, reclaimable: 10, reclaimable_resolved: 10, unknown: change.unknown ?? 0, pinned_unreferenced: change.pinned ?? 0, extents: 1, inline: 0 },
  } as unknown as ExploreEntry
}

describe('notes about one row', () => {
  it('has nothing to say about a plain file, so it opens in File Station directly', () => {
    expect(entryNotes(entry())).toEqual([])
    expect(entryNotes(entry({ pinned: null }))).toEqual([])
  })

  it('names what holds the row, its unresolved and partly used bytes, links and encryption', () => {
    expect(entryNotes(entry({
      blockers: ['snapshot', 'hard_link'],
      unknown: 4096,
      pinned: 8192,
      nlink: 2,
      flags: ['encrypted'],
    }))).toEqual(['snapshot', 'hard_link', 'unresolved_references', 'pinned_unreferenced', 'hard_links', 'unsupported_encrypted'])
  })

  it('says unresolved bytes once, not also as an unknown holder', () => {
    expect(entryNotes(entry({ blockers: ['unknown'], unknown: 4096 }))).toEqual(['unresolved_references'])
    expect(entryNotes(entry({ blockers: ['unknown'] }))).toEqual(['unknown'])
  })

  it('leaves report-wide notes and plain flags to the page and the type column', () => {
    expect(entryNotes(entry({ flags: ['snapshot_not_read', 'system', 'subvolume_root'] }))).toEqual([])
  })
})

describe('why cleanup refuses a row', () => {
  it('names the protection, as the NAS guard has it', () => {
    expect(protectedKind('/volume1/@reclaimdup-build', '/volume1', [])).toBe('dsm_folder')
    expect(protectedKind('/volume1/main/#recycle', '/volume1/main', [])).toBe('recycle_bin')
    expect(protectedKind('/volume1/main', '/volume1', ['subvolume_root'])).toBe('share_root')
    expect(protectedKind('/volume1/@synopkgland', '/volume1', ['own_state'])).toBe('own_state')
    expect(protectedKind('/volume1/@tmp', '/volume1', ['system'])).toBe('dsm_folder')
    expect(protectedKind('/volume1/main/@reclaimdup-build', '/volume1', [])).toBe('dsm_folder')
    expect(protectedKind('/volume1/main/@reclaimdup-build/done', '/volume1', [])).toBe('dsm_folder')
    expect(protectedKind('/volume1/main/src/@eaDir', '/volume1', [])).toBe('dsm_folder')
    expect(protectedKind('/volume1/main/a/b', '/volume1', [])).toBe(null)
  })
})
