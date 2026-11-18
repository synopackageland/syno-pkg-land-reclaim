import { createDuplicatesMock, mockId, type DuplicatesMockOptions } from '@/api/duplicatesFixture'
import type { DuplicateCopiesDocument, DuplicateGroup, DuplicateList, DuplicateScopeDocument } from '@/types/duplicates'
import { checkGroup, copyBlock, deletedForGood, offShares, copyKind, copyLocation, duplicateRow, filterCategory, groupSelection, keepPlan, mergeRows, mergedMember, removalOf, rowDetail, shareOf, sharePath, stays } from './duplicatesModel'
import { copy, type MockGroup } from '@/api/duplicatesFixture'

const VOLUME = '/volume1'

async function scopeOf(share: string | null, list: DuplicateList = 'files', options: DuplicatesMockOptions = {}, mock = createDuplicatesMock(options)) {
  const path = share ? `${VOLUME}/${share}` : undefined
  const answer = await mock.answer('duplicates.scope', { volume: VOLUME, ...(path ? { path } : {}), list, limit: 200 }) as DuplicateScopeDocument
  return { mock, answer }
}

async function groupIn(share: string | null, key: string, options: DuplicatesMockOptions & { list?: DuplicateList } = {}) {
  const { list = 'files', ...rest } = options
  const { mock, answer } = await scopeOf(share, list, rest)
  const id = mockId(key, list === 'folders' ? 'folder' : 'file')
  return { mock, answer, group: answer.groups.items.find(entry => entry.group.id === id)! }
}

const copiesOf = async (mock: ReturnType<typeof createDuplicatesMock>, key: string, scope: string) =>
  await mock.answer('duplicates.copies', { volume: VOLUME, group_id: mockId(key), scope: `${VOLUME}/${scope}`, limit: 200 }) as DuplicateCopiesDocument

describe('duplicates fixture', () => {
  it('keeps the copy in a preferred share first', async () => {
    const { group } = await groupIn('Archive', 'wedding', { list: 'folders' })
    expect(group.keeper).toMatchObject({ share: 'photo', reason: 'preferred_share', in_scope: false })
    expect(group.in_scope.map(item => item.path.path)).toEqual(['/volume1/Archive/wedding-2022'])
  })

  it('keeps the oldest copy on the whole volume and moves every other one, whatever its share', async () => {
    const { group } = await groupIn('Archive', 'ubuntu', { preferred: [] })
    expect(group.keeper.reason).toBe('oldest')
    expect(group.in_scope).toHaveLength(5)
    expect(group.removes).toBe(group.group.copies_total - 1 - group.in_identical_folders)
    expect(group.remove_shares).toEqual(expect.arrayContaining(['Archive', 'homes', 'public']))
  })

  it('keeps a folder right below the volume: it is never moved', async () => {
    const groups: MockGroup[] = [{ key: 'iso', kind: 'folder', name: 'iso-set', size: 4 * 1024 ** 3, files: 3, copies: [
      copy('/upgrade_test_shared', '2026-05-01'),
      copy('/Archive/iso-set', '2026-04-01'),
      copy('/video/iso-set', '2026-04-02'),
    ] }]
    const { answer } = await scopeOf('Archive', 'folders', { groups, preferred: [] })
    const group = answer.groups.items[0]!
    expect(group.keeper).toMatchObject({ reason: 'unmovable', path: { path: '/volume1/upgrade_test_shared' } })
    expect(group.removes).toBe(2)
    expect(group.remove_shares).toEqual(['Archive', 'video'])
  })

  it('keeps the oldest copy in the scope when every copy is there', async () => {
    const { group } = await groupIn('Archive', 'render')
    expect(group.keeper).toMatchObject({ reason: 'oldest', in_scope: true, path: { path: '/volume1/Archive/projects/2025/render-final.mov' } })
    expect(group.in_scope_total).toBe(2)
    expect(group.can_free).toBe(2 * group.group.size)
  })

  it('makes a user Keep the kept copy, and frees nothing for a second one', async () => {
    const { mock, group: before } = await groupIn('Archive', 'render')
    const keep = async (path: DuplicateGroup['in_scope'][number]['path']) =>
      (await mock.answer('duplicates.keep', { volume: VOLUME, group_id: before.group.id, path, keep: true, scope: `${VOLUME}/Archive` }) as { group: DuplicateGroup }).group
    const [first, second] = before.in_scope.map(item => item.path)
    const once = await keep(first!)
    expect(once.keeper).toMatchObject({ reason: 'user', path: first })
    expect(once.can_free).toBe(before.can_free)
    const twice = await keep(second!)
    expect(twice.in_scope.find(item => item.path.path === second!.path)!.role).toBe('keep_user')
    expect(twice.can_free).toBe(before.can_free - before.group.size)
    expect(groupSelection(twice, VOLUME).copies).toBe(1)
  })

  it('says recalculating after preferred shares change, until the analysis has run', async () => {
    const mock = createDuplicatesMock({ analyzeMs: 60_000 })
    const changed = await mock.answer('duplicates.preferred_shares', { volume: VOLUME, shares: ['Archive'] }) as { shares: string[]; recalculating: boolean }
    expect(changed).toMatchObject({ shares: ['Archive'], recalculating: true })
    await mock.answer('duplicates.analyze', { volume: VOLUME })
    const { answer } = await scopeOf(null, 'files', {}, mock)
    expect(answer.recalculating).toBe(true)
    expect(answer.analysis?.running).toBe(true)
  })

  it('answers none for an analysis of the older format, and a failed document without duplicate data', async () => {
    expect((await scopeOf(null, 'files', { analysis: 'none' })).answer.analysis?.state).toBe('none')
    const failed = (await scopeOf(null, 'files', { analysis: 'unavailable' })).answer
    expect(failed.completion).toBe('failed')
    expect(failed.errors?.[0]?.code).toBe('duplicates_unavailable')
  })

  it('lists every copy with its role, a copy inside an identical folder as in_folder', async () => {
    const mock = createDuplicatesMock({ preferred: [] })
    const ubuntu = await copiesOf(mock, 'ubuntu', 'Archive')
    expect(ubuntu.copies.total).toBe(20)
    const roles = ubuntu.copies.items.map(item => item.role)
    expect(roles.filter(role => role === 'keep')).toHaveLength(1)
    expect(roles.filter(role => role === 'remove')).toHaveLength(19 - roles.filter(role => role === 'in_folder').length)
    const vows = await copiesOf(createDuplicatesMock(), 'vows', 'homes')
    expect(vows.copies.items.map(item => [item.path.path, item.role])).toEqual([
      ['/volume1/homes/bob/Videos/vows.mov', 'remove'],
      ['/volume1/photo/2022/wedding/vows.mov', 'keep'],
      ['/volume1/Archive/wedding-2022/vows.mov', 'in_folder'],
    ])
  })
})

describe('duplicates model', () => {
  it('shows paths from the shared folder, and the whole path when a copy has another name', async () => {
    const { group } = await groupIn('Archive', 'dsc0192')
    expect(sharePath('/volume1/photo/a.jpg', '/volume1')).toBe('/photo/a.jpg')
    expect(copyLocation({ path: '/volume1/Archive/raw/DSC_0192 (1).ARW' }, group.group, VOLUME)).toBe('/Archive/raw')
    expect(copyLocation({ path: '/volume1/photo/2025/raw/DSC_0192.ARW' }, group.group, VOLUME)).toBe('/photo/2025/raw/DSC_0192.ARW')
    expect(duplicateRow(group, VOLUME, 'Archive').keptText).toBe('/photo/2025/raw/DSC_0192.ARW')
  })

  it('counts a row in copies and names where they are', async () => {
    const ubuntu = duplicateRow((await groupIn('Archive', 'ubuntu', { preferred: [] })).group, VOLUME, 'Archive')
    expect(ubuntu.copiesText).toBe('20 copies')
    expect(ubuntu.copiesWhere).toBe('6 here · 14 elsewhere')
    expect(ubuntu.hereText).toBe('6 copies in Archive')
    const render = duplicateRow((await groupIn('Archive', 'render')).group, VOLUME, 'Archive')
    expect(render.copiesWhere).toBe('all in Archive')
    expect(render.keptWhy).toContain('Oldest copy')
  })

  it('counts copies inside identical folders apart from here and elsewhere', async () => {
    const { group } = await groupIn('homes', 'vows')
    expect(group.in_identical_folders).toBe(1)
    expect(duplicateRow(group, VOLUME, 'homes').copiesWhere).toBe('1 here · 1 elsewhere · 1 in identical folders')
  })

  it('puts a group in the cleanup list with what Review needs', async () => {
    const { group } = await groupIn('Archive', 'family')
    expect(groupSelection(group, VOLUME)).toEqual({
      id: mockId('family'),
      name: 'family-2019.mkv',
      copies: 1,
      canFree: group.can_free,
      freesLater: group.frees_later,
      keeps: '/video/home-movies/family-2019.mkv',
      preferred: false,
      drive: false,
      shares: group.remove_shares,
    })
    expect(group.frees_later).toBe(group.group.size)
  })

  it('maps the filters to the API categories, folders under All only', () => {
    expect(filterCategory('all')).toBeUndefined()
    expect(filterCategory('photo')).toBe('photos')
    expect(filterCategory('video')).toBe('videos')
    expect(filterCategory('music')).toBe('music')
  })

  it('names the kept copy wherever it is: Reclaim is for administrators only', async () => {
    const { group } = await groupIn('Archive', 'scans', { preferred: [] })
    expect(group.keeper).toMatchObject({ path: { path: '/volume1/private/finance/tax-scans-2018.zip' }, share: 'private', reason: 'oldest', in_scope: false })
    expect(group.keeper).not.toHaveProperty('hidden')
    expect(group).not.toHaveProperty('elsewhere_hidden')
    const row = duplicateRow(group, VOLUME, 'Archive')
    expect(row.keptText).toBe('/private/finance')
    expect(groupSelection(group, VOLUME)).toMatchObject({ keeps: '/private/finance/tax-scans-2018.zip', copies: group.removes })
  })

  it('lists every copy of a group, in every share', async () => {
    const { mock, group } = await groupIn('Archive', 'img4471')
    expect(group.group.copies_total).toBe(6)
    expect(group.elsewhere_by_share).toContainEqual({ share: 'private', count: 1 })
    const page = await copiesOf(mock, 'img4471', 'Archive')
    expect(page.copies.total).toBe(6)
    expect(page).not.toHaveProperty('hidden')
    expect(page.copies.items.map(item => item.share)).toContain('private')
  })

  it('shows a small-file folder group with its file count', async () => {
    const { group } = await groupIn('Archive', 'src', { list: 'folders' })
    expect(group.group).toMatchObject({ kind: 'folder', files: 48_213 })
    expect(duplicateRow(group, VOLUME, 'Archive').hereText).toBe('/Archive/old-mac/dev/reclaim-src · 48,213 files')
  })

  it('shows partly identical folders from the side removed: what is the same, and as what', async () => {
    const { group } = await groupIn('homes', 'linux', { list: 'folders' })
    expect(group.group).toMatchObject({ kind: 'folder_partial', same_items: 38 })
    expect(group.group.id).toBe(mockId('linux', 'folder_partial'))
    expect(group.group.id.startsWith('f')).toBe(true)
    expect(group.partial).toMatchObject({ path: { path: '/volume1/homes/alice/src/linux-6.6' }, same_items: 38, total_items: 39, counterpart: { path: '/volume1/Archive/src/linux-6.6-copy' } })
    expect(group.partial!.total_bytes).toBeGreaterThan(group.partial!.same_bytes)
    const row = duplicateRow(group, VOLUME, 'homes')
    expect(row.name).toBe('linux-6.6')
    expect(row.hereText).toBe('38 of 39 items same as /Archive/src/linux-6.6-copy')
    expect(row.copiesText).toBe('2 folders')
    expect(row.sizeText).toMatch(/ identical in each$/)
    expect(row.canFree).toBe(group.group.size)
    expect(copyKind(group.group)).toBe('dir')
    expect(groupSelection(group, VOLUME)).toMatchObject({ name: 'linux-6.6', copies: 1, keeps: '/Archive/src/linux-6.6-copy', partial: { items: 38 } })
  })

  it('names the side removed and the kept side on any share page: the group is the same', async () => {
    const { group } = await groupIn('Archive', 'linux', { list: 'folders' })
    expect(duplicateRow(group, VOLUME, 'Archive')).toMatchObject({ name: 'linux-6.6', hereText: '38 of 39 items same as /Archive/src/linux-6.6-copy' })
  })

  it('says under a merged row how many groups it holds, which the cleanup list counts', async () => {
    const { answer } = await scopeOf('homes', 'folders')
    const rows = mergeRows(answer.groups.items.map(group => duplicateRow(group, VOLUME, 'homes')), VOLUME)
    const merged = rows.find(row => row.members.length > 1)!
    expect(rowDetail(merged)).toBe('2 folders match it: linux-6.6 · linux-6.6-copy-env · Includes 2 groups')
    for (const row of rows.filter(entry => entry.members.length === 1)) expect(rowDetail(row)).not.toContain('Includes')
    // A merged row of whole folders: its folder count, then its groups, with no empty part between.
    const whole = { ...merged, group: { ...merged.group, partial: undefined } }
    expect(rowDetail(whole)).toBe('3 folders · Includes 2 groups')
  })

  it('merges the rows of folders that match the same kept folder, with each one\'s match and date', async () => {
    const { answer } = await scopeOf('homes', 'folders')
    const rows = mergeRows(answer.groups.items.map(group => duplicateRow(group, VOLUME, 'homes')), VOLUME)
    const merged = rows.filter(row => row.members.length > 1)
    expect(merged).toHaveLength(1)
    const row = merged[0]!
    expect(row.members.map(group => group.partial!.path.path).sort()).toEqual(['/volume1/homes/alice/src/linux-6.6', '/volume1/homes/alice/src/linux-6.6-copy-env'])
    expect(row).toMatchObject({ name: 'linux-6.6-copy', keptText: '/Archive/src/linux-6.6-copy', removes: 2 })
    expect(row.hereText).toBe('2 folders match it: linux-6.6 · linux-6.6-copy-env')
    expect(row.canFree).toBe(row.members.reduce((sum, group) => sum + group.can_free, 0))
    const env = mergedMember(row.members.find(group => group.group.name.path === 'linux-6.6-copy-env')!)
    expect(env.same).toBe('93.5% same · 38 of 40 items')
    expect(env.mtime).toBe('2026-03-01T12:00:00Z')
    // Rows that keep different copies stay apart, and files never merge.
    expect(rows.length).toBe(answer.groups.items.length - 1)
  })

  it('lists no partly identical folders among files, nor under a category', async () => {
    const { answer } = await scopeOf('homes', 'files')
    expect(answer.groups.items.some(item => item.group.kind === 'folder_partial')).toBe(false)
  })

  it('pages the identical items in the copies answer, largest first', async () => {
    const mock = createDuplicatesMock()
    const page = async (offset: number) => (await mock.answer('duplicates.copies', { volume: VOLUME, group_id: mockId('linux', 'folder_partial'), scope: `${VOLUME}/homes`, offset, limit: 30 }) as DuplicateCopiesDocument)
    const first = await page(0)
    expect(first.copies.total).toBe(2)
    expect(first.items).toMatchObject({ total: 38, offset: 0 })
    expect(first.items!.items).toHaveLength(30)
    expect(first.items!.items[0]).toMatchObject({ path: { path: 'drivers' }, kind: 'folder' })
    const next = await page(30)
    expect(next.items!.items).toHaveLength(8)
    expect(next.items!.items.every(item => item.kind === 'file')).toBe(true)
    expect((await copiesOf(mock, 'ubuntu', 'Archive')).items).toBeUndefined()
  })
})

describe('checkGroup', () => {
  const bins = (entries: [string, boolean][]) => new Map(entries.map(([name, enabled]) => [name, { enabled }]))

  it('lets a group that removes a copy from a share without a Recycle Bin be selected, and names that share', async () => {
    const { mock, group } = await groupIn('Archive', 'ubuntu', { preferred: [] })
    const shares = bins(group.remove_shares.map(share => [share, share !== 'public']))
    expect(checkGroup(group).block).toBeNull()
    expect(offShares(group, shares)).toEqual(['public'])
    expect(offShares(group, bins(group.remove_shares.map(share => [share, true])))).toEqual([])
    expect(offShares(group, null)).toEqual([])
    // Its copies there are deleted for good; the others still move to a Recycle Bin.
    const { copies } = await copiesOf(mock, 'ubuntu', 'Archive')
    for (const item of copies.items) expect(deletedForGood(item, shares)).toBe(item.share === 'public')
  })

  it('words a removal by the Recycle Bins of the shared folders it removes from', () => {
    const shares = bins([['photos', true], ['public', false]])
    expect(shareOf('/volume1/public/a/b.iso', '/volume1')).toBe('public')
    expect(removalOf(['photos'], shares)).toBe('move')
    expect(removalOf(['public', 'public'], shares)).toBe('delete')
    expect(removalOf(['photos', 'public'], shares)).toBe('mixed')
    // Outside every shared folder there is no Recycle Bin either.
    expect(removalOf([null], shares)).toBe('delete')
    // Before the Recycle Bins are read, or with nothing chosen, it is the move it always was.
    expect(removalOf(['public'], null)).toBe('move')
    expect(removalOf([], shares)).toBe('move')
  })

  it('lets copies in a folder DSM does not list be selected: Review deletes them for good once confirmed', async () => {
    const groups: MockGroup[] = [{ key: 'tar', kind: 'file', name: 'package-meta.tar', size: 20 * 1024 ** 2, copies: [
      copy('/restore-test/a', '2026-01-01'),
      copy('/restore-test/b', '2026-01-02'),
    ] }]
    const { mock, answer } = await scopeOf(null, 'files', { groups })
    const group = answer.groups.items[0]!
    const shares = bins([['Archive', true]])
    expect(checkGroup(group).block).toBeNull()
    const { copies } = await mock.answer('duplicates.copies', { volume: VOLUME, group_id: mockId('tar'), limit: 200 }) as DuplicateCopiesDocument
    expect(copies.items.map(item => deletedForGood(item, shares))).toEqual([true, true])
    expect(deletedForGood(copies.items[0]!, bins([['restore-test', true]]))).toBe(false)
    expect(deletedForGood(copies.items[0]!, null)).toBe(false)
  })

  it('lists the copies that stay because they are right below the volume', async () => {
    const groups: MockGroup[] = [{ key: 'iso', kind: 'folder', name: 'iso-set', size: 1024, files: 1, copies: [
      copy('/upgrade_test_shared', '2026-05-01'),
      copy('/Archive/iso-set', '2026-04-01'),
    ] }]
    const { answer } = await scopeOf('Archive', 'folders', { groups })
    const result = checkGroup(answer.groups.items[0]!)
    expect(result.unmovable.map(path => path.path)).toEqual(['/volume1/upgrade_test_shared'])
    expect(result.block).toBeNull()
  })
})

describe('keepPlan', () => {
  const keys = (...paths: string[]) => new Set(paths)
  const sent = (changes: ReturnType<typeof keepPlan>) => changes?.map(change => `${change.keep ? '+' : '-'}${change.path.path}`) ?? null

  it('stores nothing when only the automatic copy is left unticked', async () => {
    const { mock, group } = await groupIn('Archive', 'render')
    const { copies } = await copiesOf(mock, 'render', 'Archive')
    expect(sent(keepPlan(group, copies.items, keys(group.keeper.path.path), VOLUME))).toEqual([])
  })

  it('keeps exactly the copies left unticked, the automatic one too when it is one of them', async () => {
    const { mock, group } = await groupIn('Archive', 'ubuntu', { preferred: [] })
    const { copies } = await copiesOf(mock, 'ubuntu', 'Archive')
    const other = copies.items.find(copy => copy.role === 'remove')!
    expect(sent(keepPlan(group, copies.items, keys(group.keeper.path.path, other.path.path), VOLUME))?.sort()).toEqual([`+${group.keeper.path.path}`, `+${other.path.path}`].sort())
    // Only another copy: the automatic one moves.
    expect(sent(keepPlan(group, copies.items, keys(other.path.path), VOLUME))).toEqual([`+${other.path.path}`])
  })

  it('takes back a copy the user kept once it is ticked again', async () => {
    const { mock } = await groupIn('Archive', 'render')
    const first = (await copiesOf(mock, 'render', 'Archive')).copies.items.find(copy => copy.role === 'remove')!
    await mock.answer('duplicates.keep', { volume: VOLUME, group_id: mockId('render'), path: first.path, keep: true })
    const group = (await scopeOf('Archive', 'files', {}, mock)).answer.groups.items.find(entry => entry.group.id === mockId('render'))!
    const { copies } = await copiesOf(mock, 'render', 'Archive')
    expect(group.keeper.reason).toBe('user')
    const other = copies.items.find(copy => copy.path.path !== first.path.path)!
    expect(sent(keepPlan(group, copies.items, keys(other.path.path), VOLUME))).toEqual([`+${other.path.path}`, `-${first.path.path}`])
  })

  it('refuses to move every copy, and keeps one that never moves by name', async () => {
    const { mock, group } = await groupIn('Archive', 'render')
    const { copies } = await copiesOf(mock, 'render', 'Archive')
    expect(keepPlan(group, copies.items, keys(), VOLUME)).toBeNull()
    const fixed = { ...copies.items[0]!, path: { path: '/volume1/render-final.mov' }, role: 'remove' as const }
    expect(stays(fixed, group, VOLUME)).toBe(true)
    expect(sent(keepPlan(group, [...copies.items, fixed], keys(), VOLUME))).toEqual(['+/volume1/render-final.mov'])
  })
})

describe('copyBlock', () => {
  it('leaves a copy in a share without a Recycle Bin choosable: only staying or moving with its folder takes the box away', async () => {
    const { mock, group } = await groupIn('Archive', 'render')
    const { copies } = await copiesOf(mock, 'render', 'Archive')
    const inside = { ...copies.items[0]!, path: { path: `/volume1/${copies.items[0]!.share}/old/render.mov` }, role: 'in_folder' as const }
    expect(copyBlock(inside, group, VOLUME)).toBe('inFolder')
    const removed = copies.items.find(copy => copy.role === 'remove')!
    expect(copyBlock(removed, group, VOLUME)).toBeNull()
  })
})
