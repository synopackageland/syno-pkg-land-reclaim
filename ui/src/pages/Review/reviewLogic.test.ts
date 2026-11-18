import { CHUNK_BYTES, basketPaths, cleanupApplyParams, conflictResolution, duplicateReview, duplicatesParams, planParams } from './reviewLogic'

describe('cleanup review logic', () => {
  it('sends only selected basket path fields to cleanup.plan', () => {
    expect(basketPaths([
      { path: '/volume1/share/a', reclaimable: 99 },
      { path: '/volume1/share/�', path_hex: '2f766f6c756d65312fff', reclaimable: null },
    ])).toEqual([
      { path: '/volume1/share/a' },
      { path: '/volume1/share/�', path_hex: '2f766f6c756d65312fff' },
    ])
  })

  it('carries report identity with exact path bytes and optional generation', () => {
    expect(basketPaths([{ path: '/volume1/�', path_hex: '2f766f6c756d65312fff', reclaimable: 99,
      expect: { filesystem_uuid: 'fs', tree: 256, objectid: 300, generation: 9 } }])).toEqual([
      { path: '/volume1/�', path_hex: '2f766f6c756d65312fff', expect: { filesystem_uuid: 'fs', tree: 256, objectid: 300, generation: 9 } },
    ])
  })

  it('applies only a sealed plan id and the move confirmation', () => {
    expect(cleanupApplyParams('plan-123')).toEqual({ plan_id: 'plan-123', confirm_move: true })
    expect(cleanupApplyParams('plan-123', true)).toEqual({ plan_id: 'plan-123', confirm_move: true, confirm_delete: true })
  })

  it('sums the Duplicates groups for Review: kept copy, snapshot bytes and Drive', () => {
    const group = { id: 'g', name: 'a.iso', copies: 2, canFree: 10, freesLater: 10, keeps: '/software/a.iso', preferred: true, drive: true }
    const summary = duplicateReview([group, { ...group, id: 'h', name: 'b.bin', copies: 1, freesLater: 0, keeps: '/private/b.bin', preferred: false, drive: false }])
    expect(summary.lines).toEqual([
      { group: 'g', name: 'a.iso', removes: 2, keeps: '/software/a.iso', preferred: true, removal: 'move' },
      { group: 'h', name: 'b.bin', removes: 1, keeps: '/private/b.bin', preferred: false, removal: 'move' },
    ])
    expect(summary).toMatchObject({ freesLater: 10, touchesDrive: true })
    expect(summary).not.toHaveProperty('untouched')
    expect(duplicateReview([])).toMatchObject({ lines: [], touchesDrive: false, freesLater: 0 })
  })

  it('says whether a group\'s copies move to the Recycle Bin or are deleted for good, from the plan\'s shared folders', () => {
    const group = { id: 'g', name: 'a.iso', copies: 2, canFree: 10, freesLater: 0, keeps: '/main/a.iso', preferred: false, drive: false }
    const removal = (shares: string[] | undefined, deletes: string[]) =>
      duplicateReview([{ ...group, ...(shares ? { shares } : {}) }], new Set(deletes)).lines[0]!.removal
    expect(removal(['media'], [])).toBe('move')
    expect(removal(['photos'], ['media'])).toBe('move')
    expect(removal(['media'], ['media'])).toBe('delete')
    expect(removal(['archive', 'media'], ['archive', 'media'])).toBe('delete')
    expect(removal(['main', 'media'], ['media'])).toBe('mixed')
    // A group that does not name its shares may have a copy in one with no Recycle Bin.
    expect(removal(undefined, ['media'])).toBe('mixed')
  })

  it('marks the Review lines of partly identical folders with their identical items', () => {
    const group = { id: 'f'.repeat(32), name: 'linux-6.6', copies: 1, canFree: 10, freesLater: 0, keeps: '/Archive/src/linux-6.6-copy', preferred: false, drive: false }
    const summary = duplicateReview([{ ...group, partial: { items: 38 } }, { ...group, id: 'a'.repeat(32), name: 'a.iso' }])
    expect(summary.lines.map(line => line.items)).toEqual([38, undefined])
    expect(summary.partial).toBe(true)
    expect(duplicateReview([group]).partial).toBe(false)
  })

  it('sends a duplicates selection as group ids of the volume, with no scope, or nothing', () => {
    const group = { id: 'a'.repeat(32), name: 'a', copies: 1, canFree: 1, freesLater: 0, keeps: '/a', preferred: false, drive: false }
    // The plan sees every group on its whole volume: a scope would be refused as an unknown field.
    expect(duplicatesParams('/volume1', { groups: [group] })).toEqual({ duplicates: { volume: '/volume1', groups: ['a'.repeat(32)] } })
    expect(duplicatesParams('/volume1', { groups: [] })).toEqual({})
    expect(duplicatesParams('/volume1', null)).toEqual({})
  })
})

describe('planParams', () => {
  const item = (i: number) => ({ path: `/volume1/photos/2026/${'long-folder-name/'.repeat(4)}IMG_${String(i).padStart(5, '0')}.jpg`, reclaimable: 1, expect: { filesystem_uuid: '00112233-4455-6677-8899-aabbccddeeff', tree: 256, objectid: 1000 + i } })

  it('sends a selection that fits one request as it is', async () => {
    const stage = vi.fn()
    const params = await planParams([item(1)], '/volume1', null, stage)
    expect(stage).not.toHaveBeenCalled()
    expect(params.paths?.[0]?.path).toBe(item(1).path)
  })

  it('stages 50,000 items chunk by chunk, each request within the bound, in order', async () => {
    const items = Array.from({ length: 50_000 }, (_, i) => item(i))
    const groups = Array.from({ length: 300 }, (_, i) => ({ id: i.toString(16).padStart(32, '0'), name: 'a', copies: 1, canFree: 1, freesLater: 0, keeps: '/k', preferred: false, drive: false }))
    const sent: { selection?: string; paths?: { path: string }[]; groups?: string[] }[] = []
    const stage = vi.fn(async (chunk: (typeof sent)[number]) => {
      expect(JSON.stringify(chunk).length).toBeLessThanOrEqual(CHUNK_BYTES)
      sent.push(chunk)
      return { selection: 'selection-18e-4f' }
    })
    const params = await planParams(items, '/volume1', { groups }, stage)
    expect(params).toEqual({ selection: 'selection-18e-4f', duplicates: { volume: '/volume1', groups: [] } })
    expect(sent[0]!.selection).toBeUndefined()
    expect(sent.slice(1).every(chunk => chunk.selection === 'selection-18e-4f')).toBe(true)
    expect(sent.flatMap(chunk => chunk.paths ?? []).map(path => path.path)).toEqual(items.map(i => i.path))
    expect(sent.flatMap(chunk => chunk.groups ?? [])).toEqual(groups.map(group => group.id))
    expect(sent.length).toBeGreaterThan(100)
  })

  it('stops between chunks once the review is closed', async () => {
    const controller = new AbortController()
    const stage = vi.fn(async () => { controller.abort(); return { selection: 'selection-1-2' } })
    await expect(planParams(Array.from({ length: 2000 }, (_, i) => item(i)), '/volume1', null, stage, controller.signal)).rejects.toThrow()
    expect(stage).toHaveBeenCalledTimes(1)
  })
})

describe('conflictResolution', () => {
  const g = (id: string) => ({ id, name: { path: id } })
  const conflict = (kept: string, removing: string) => ({ path: '/volume1/x', conflict: { kept_in: g(kept), removed_by: g(removing) } })

  it('takes out the group that removes the kept copy, once', () => {
    const result = conflictResolution([conflict('env', 'junk'), conflict('other', 'junk'), conflict('env', 'scsi')], new Set(['env', 'junk', 'scsi']))
    expect(result.drop).toEqual([{ id: 'junk', name: 'junk' }, { id: 'scsi', name: 'scsi' }])
    expect(result.choices).toEqual([])
  })

  it('leaves groups that are not in the list alone', () => {
    expect(conflictResolution([conflict('env', 'junk'), { path: '/volume1/y' }], new Set(['env']))).toEqual({ drop: [], choices: [] })
  })

  it('asks which of two listed groups that remove each other\'s kept copy goes', () => {
    const result = conflictResolution([conflict('a', 'b'), conflict('b', 'a')], new Set(['a', 'b']))
    expect(result.drop).toEqual([])
    expect(result.choices).toEqual([[{ id: 'a', name: 'a' }, { id: 'b', name: 'b' }]])
  })

  it('asks nothing when one of the pair goes anyway', () => {
    const result = conflictResolution([conflict('a', 'b'), conflict('b', 'a'), conflict('c', 'b')], new Set(['a', 'b', 'c']))
    expect(result.choices).toEqual([])
  })
})
