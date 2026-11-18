import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { fixtureBackend } from '@/api/fixtures'
import { createDuplicatesMock, mockId, type DuplicatesMockOptions } from '@/api/duplicatesFixture'
import { setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import App from '@/App.vue'
import { CellText } from '@synopackageland/ui'
import './DuplicatesPage.vue'
import { allowAuthorizationForTests } from '@/authorizationFixture'
import { nav } from '@/stores/nav'
import { session } from '@/stores/session'
import { useBasket } from '@/stores/basket'
import { volumes } from '@/stores/volumes'
import { setRecycleBinSource } from '@/dsm/recycleBins'

enableAutoUnmount(afterEach)

async function settle() {
  for (let i = 0; i < 3; i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
}

type Mock = ReturnType<typeof createDuplicatesMock>

async function mountDuplicates(options: DuplicatesMockOptions = {}, prepare?: (mock: Mock) => Promise<unknown>, jobList?: unknown[], keepBins = false) {
  sessionStorage.clear()
  if (!keepBins) setRecycleBinSource(null)
  allowAuthorizationForTests()
  nav.page = 'duplicates'
  session.status = 'loading'
  volumes.document = null
  volumes.selected = null
  useBasket('/volume1').clear()
  const mock = createDuplicatesMock(options)
  await prepare?.(mock)
  const base = fixtureBackend()
  const calls: string[] = []
  setBackend(((method, params, request) => {
    if (method === 'jobs.list' && jobList) return Promise.resolve({ jobs: jobList })
    const analysisJob = method === 'job.result' && String((params as { job_id?: string })?.job_id).startsWith('duplicates-analyze')
    if (!method.startsWith('duplicates.') && !analysisJob) return request ? base(method, params, request) : base(method, params)
    calls.push(method)
    return mock.answer(method, params)
  }) as Backend)
  const wrapper = mount(App, { attachTo: document.body })
  await settle()
  return { wrapper, mock, calls }
}

type Wrapper = Awaited<ReturnType<typeof mountDuplicates>>['wrapper']

/** Opens a share on one of its two lists: its large files unless said otherwise. */
async function openShare(wrapper: Wrapper, share = 'Archive', list: 'folders' | 'files' = 'files') {
  await wrapper.get(`[data-testid="duplicates-share-${share}"]`).trigger('click')
  await settle()
  await showList(wrapper, list)
}

async function showList(wrapper: Wrapper, list: 'folders' | 'files') {
  const label = list === 'folders' ? 'Identical folders' : 'Big duplicate files'
  const tab = wrapper.findAll('[role="tab"]').find(node => node.text().startsWith(label))!
  await tab.trigger('mousedown', { button: 0 })
  await tab.trigger('click')
  await settle()
}

const rowOf = (wrapper: Wrapper, name: string) =>
  wrapper.findAll('tbody tr').find(row => row.text().includes(name))!

const basketGroups = () => useBasket('/volume1').duplicates.value

/** Opens a row's copies in the sheet. */
async function openGroup(wrapper: Wrapper, name: string) {
  await rowOf(wrapper, name).find('td:last-child').trigger('click')
  await settle()
}
const sheetCopies = (state: string) => [...document.body.querySelectorAll<HTMLElement>(`[data-testid="keep-copy-${state}"]`)]
const byTestId = (testid: string) => document.body.querySelector<HTMLElement>(`[data-testid="${testid}"]`)
async function clickTestId(testid: string) {
  byTestId(testid)!.click()
  await settle()
}
/** Every share of the fixture, its Recycle Bin on unless named off. */
function binsWith(off: string[] = []) {
  setRecycleBinSource(() => Promise.resolve(new Map(['Archive', 'homes', 'music', 'photo', 'private', 'public', 'software', 'video']
    .map(name => [name, { enabled: !off.includes(name), adminOnly: false }]))))
}
const sheetText = (testid: string) => document.body.querySelector(`[data-testid="${testid}"]`)?.textContent?.replace(/\s+/g, ' ').trim() ?? ''
async function tick(copy: HTMLElement) {
  copy.querySelector<HTMLElement>('[role="checkbox"]')!.click()
  await settle()
}

describe('DuplicatesPage', () => {
  it('lists the shares of the volume with what each can free', async () => {
    const { wrapper } = await mountDuplicates()
    expect(wrapper.get('[data-testid="duplicates-scope-title"]').text()).toMatch(/^volume1 · can free /)
    expect(wrapper.get('[data-testid="duplicates-scope-detail"]').text()).toContain('counts in each')
    expect(wrapper.find('[data-testid="duplicates-share-Archive"]').exists()).toBe(true)
    expect(wrapper.get('[data-testid="duplicates-select-all"]').text()).toBe('Select all duplicates on volume1')
    const notes = (wrapper.findComponent({ name: 'PageInfo' }).props('notes') as string[]).join(' ')
    expect(notes).toContain('.DS_Store, Thumbs.db, desktop.ini, ._ files, @eaDir')
    expect(notes).not.toMatch(/similar/i)
  })

  it('selects every duplicate on the volume from its overview', async () => {
    const { wrapper } = await mountDuplicates()
    await wrapper.get('[data-testid="duplicates-select-all"]').trigger('click')
    await settle()
    expect(basketGroups().groups.length).toBeGreaterThan(0)
    expect(basketGroups().groups.map(group => group.id)).toContain(mockId('ubuntu'))
  })

  it('selects groups in a share without a Recycle Bin too: Review deletes those copies for good once confirmed', async () => {
    binsWith(['Archive'])
    const { wrapper } = await mountDuplicates({}, undefined, undefined, true)
    await wrapper.get('[data-testid="duplicates-select-all"]').trigger('click')
    await settle()
    const ids = basketGroups().groups.map(group => group.id)
    expect(ids).toContain(mockId('album'))
    expect(ids).toContain(mockId('render'))
    // Nothing was skipped and nothing stays: the dialog closes by itself.
    expect(byTestId('select-all-dialog')).toBeNull()
  })

  it('counts the groups it checks and can be stopped, keeping what it had selected', async () => {
    const { wrapper } = await mountDuplicates()
    await wrapper.get('[data-testid="duplicates-select-all"]').trigger('click')
    expect(byTestId('select-all-checking')).not.toBeNull()
    await clickTestId('select-all-stop')
    expect(sheetText('select-all-stopped')).toBe('Stopped: groups already checked are selected')
    expect(byTestId('select-all-close')).not.toBeNull()
  })

  it('never selects a folder right below the volume: it stays, and says why', async () => {
    binsWith()
    const groups = [{ key: 'iso', kind: 'folder' as const, name: 'iso-set', size: 4 * 1024 ** 3, files: 3, copies: [
      { at: '/upgrade_test_shared', date: '2026-05-01' },
      { at: '/Archive/iso-set', date: '2026-04-01' },
    ] }]
    const { wrapper } = await mountDuplicates({ groups }, undefined, undefined, true)
    await wrapper.get('[data-testid="duplicates-select-all"]').trigger('click')
    await settle()
    expect(basketGroups().groups.map(group => [group.id, group.copies, group.keeps])).toEqual([[mockId('iso', 'folder'), 1, '/upgrade_test_shared']])
    expect(sheetText('select-all-stays')).toContain('/upgrade_test_shared')
    expect(sheetText('select-all-stays')).toContain('Not in a DSM shared folder, stays')
  })

  it('names the volume in the scope line, not in a breadcrumb of one level', async () => {
    const { wrapper } = await mountDuplicates()
    expect(wrapper.find('[data-testid="duplicates-breadcrumb"]').exists()).toBe(false)
    await openShare(wrapper)
    const crumbs = wrapper.get('[data-testid="duplicates-breadcrumb"]')
    expect(crumbs.text()).toContain('volume1')
    expect(crumbs.text()).toContain('Archive')
  })

  it('shows a share as one row per group with the kept copy', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper)
    const ubuntu = rowOf(wrapper, 'ubuntu-24.04.1-desktop-amd64.iso')
    expect(ubuntu.text()).toContain('20 copies')
    expect(ubuntu.text()).toContain('6 here · 14 elsewhere')
    expect(ubuntu.text()).toContain('/software/os/ubuntu')
  })

  it('names a kept copy in a preferred share', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper, 'Archive', 'folders')
    expect(rowOf(wrapper, 'wedding-2022').text()).toContain('Preferred share')
  })

  it('puts a row in the cleanup list as one group, counted in copies', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper)
    await rowOf(wrapper, 'ubuntu-24.04.1-desktop-amd64.iso').get('[role="checkbox"]').trigger('click')
    await settle()
    // Every copy on the volume but the kept one, whatever its share.
    const [selected] = basketGroups().groups
    expect(selected).toMatchObject({ id: mockId('ubuntu') })
    expect(selected!.copies).toBeGreaterThan(6)
    expect(useBasket('/volume1').items.value).toHaveLength(0)
    expect(wrapper.text()).toContain(`${selected!.copies} copies from 1 group`)
  })

  it('counts each group of a merged row: one row, but as many groups as folders match its kept folder', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper, 'homes', 'folders')
    await rowOf(wrapper, '2 folders match it').get('[role="checkbox"]').trigger('click')
    await settle()
    const groups = basketGroups().groups
    expect(groups.map(group => group.name).sort()).toEqual(['linux-6.6', 'linux-6.6-copy-env'])
    expect(wrapper.text()).toContain('Cleanup list: 2 copies from 2 groups')
  })

  it('puts what the cleanup keeps after the amount, bracketed like Explore and kept apart from it', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper, 'homes', 'folders')
    await rowOf(wrapper, '2 folders match it').get('[role="checkbox"]').trigger('click')
    await settle()
    const details = wrapper.findAll('span').find(node => node.text().includes('One copy of each group stays'))!
    // A no-break space: the bar's flex items drop a plain leading one.
    expect(details.element.textContent).toBe('\u00a0(One copy of each group stays)')
  })

  it('selects every group the filter shows and says so', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper)
    const videos = wrapper.findAll('[role="radio"], button').find(node => node.text() === 'Videos')!
    await videos.trigger('click')
    await settle()
    const selectAll = wrapper.get('[data-testid="duplicates-select-all"]')
    expect(selectAll.text()).toBe('Select all duplicate videos in Archive')
    await selectAll.trigger('click')
    await settle()
    // Groups are the whole volume's: vows.mov has a copy in Archive (inside wedding-2022), so it counts.
    expect(basketGroups().groups.map(group => group.id).sort()).toEqual([mockId('family'), mockId('img4471'), mockId('render'), mockId('vows')].sort())
  })

  it('adds groups from several shares to one cleanup list', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper)
    await rowOf(wrapper, 'render-final.mov').get('[role="checkbox"]').trigger('click')
    await settle()
    await wrapper.findAll('ol:not([aria-hidden]) button').find(node => node.text() === 'volume1')!.trigger('click')
    await settle()
    await openShare(wrapper, 'homes')
    await rowOf(wrapper, 'vows.mov').get('[role="checkbox"]').trigger('click')
    await settle()
    expect(basketGroups().groups.map(group => group.id)).toEqual([mockId('render'), mockId('vows')])
  })

  it("moves the ticked copies and keeps the unticked ones from a row's sheet", async () => {
    const { wrapper, mock } = await mountDuplicates()
    await openShare(wrapper)
    await openGroup(wrapper, 'render-final.mov')
    // One copy starts unticked, the kept one, marked with why; the others are ticked.
    expect(sheetCopies('kept')).toHaveLength(1)
    expect(sheetCopies('kept')[0]!.querySelector('[data-testid="keep-default"]')).not.toBeNull()
    expect(sheetCopies('kept')[0]!.textContent).toContain('Oldest copy')
    expect(sheetCopies('moves')).toHaveLength(2)
    expect(sheetText('keep-summary')).toBe('Moves 2 of 3 · keeps 1')
    // Untick a second copy to keep it too.
    await tick(sheetCopies('moves')[0]!)
    expect(sheetText('keep-summary')).toBe('Moves 1 of 3 · keeps 2')
    await clickTestId('keep-apply')
    expect(mock.userKeeps.get(mockId('render'))?.size).toBe(2)
    expect(basketGroups().groups[0]).toMatchObject({ id: mockId('render'), copies: 1 })
    expect(rowOf(wrapper, 'render-final.mov').text()).toContain('Keeps 2 copies')
  })

  it('keeps at least one copy: ticking them all disables Apply', async () => {
    const { wrapper, mock } = await mountDuplicates()
    await openShare(wrapper)
    await openGroup(wrapper, 'render-final.mov')
    await tick(sheetCopies('kept')[0]!)
    expect(sheetCopies('kept')).toHaveLength(0)
    expect(sheetText('keep-at-least-one')).toBe('Leave at least one copy unticked')
    expect((byTestId('keep-apply') as HTMLButtonElement).disabled).toBe(true)
    expect(mock.userKeeps.get(mockId('render'))).toBeUndefined()
  })

  it('sorts the copies by date, oldest first', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper)
    await openGroup(wrapper, 'ubuntu-24.04.1-desktop-amd64.iso')
    const dates = () => [...byTestId('keep-copies')!.querySelectorAll('li')].map(li => li.textContent ?? '')
    expect(dates()[0]).toContain('/software/os/ubuntu')
    const newest = [...byTestId('keep-order')!.querySelectorAll<HTMLElement>('[role="radio"], button')].find(node => node.textContent === 'Newest first')!
    newest.click()
    await settle()
    expect(dates().at(-1)).toContain('/software/os/ubuntu')
  })

  it('adds a group to the cleanup list from its sheet, and takes it out again', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper)
    await openGroup(wrapper, 'render-final.mov')
    await clickTestId('keep-apply')
    expect(basketGroups().groups.map(group => group.id)).toEqual([mockId('render')])
    await openGroup(wrapper, 'render-final.mov')
    await clickTestId('keep-drop')
    expect(basketGroups().groups).toEqual([])
  })

  it('lets the duplicates of a share without a Recycle Bin be selected, says they are deleted for good, and offers its Control Panel settings', async () => {
    binsWith(['Archive'])
    const { wrapper } = await mountDuplicates({}, undefined, undefined, true)
    const badge = wrapper.get('[data-testid="duplicates-bin-Archive"]')
    expect(badge.text()).toBe('Off · deleted for good')
    expect(badge.element.parentElement!.querySelector('[data-testid="share-settings-Archive"]')).not.toBeNull()
    expect(wrapper.get('[data-testid="duplicates-bin-homes"]').text()).toBe('On')
    expect(wrapper.text()).not.toContain("can't clean up")
    await openShare(wrapper)
    const row = rowOf(wrapper, 'render-final.mov')
    expect(row.text()).toContain('Deletes for good')
    expect(row.text()).not.toContain("Can't clean up")
    // The cell keeps the row's height: one line each, the settings an icon beside them.
    const moves = row.findAllComponents(CellText).find((cell: { attributes: (name: string) => string | undefined }) => cell.attributes('data-testid')?.startsWith('duplicates-moves-'))!
    expect(moves.props('wrap')).toBe(false)
    expect(row.find('[data-testid="share-settings-Archive"]').exists()).toBe(true)
    const box = row.get('[role="checkbox"]')
    expect(box.attributes('aria-disabled') === 'true' || box.attributes('disabled') !== undefined).toBe(false)
    await openGroup(wrapper, 'render-final.mov')
    expect(sheetCopies('blocked-binOff')).toHaveLength(0)
    const archived = sheetCopies('moves').filter(copy => copy.querySelector('[data-testid="share-settings-Archive"]'))
    expect(archived.length).toBeGreaterThan(0)
    expect(archived[0]!.textContent).toContain('No Recycle Bin here: removing it deletes it for good')
    // The copy that stays is not removed: it says nothing of deleting and offers no settings.
    for (const kept of sheetCopies('kept')) {
      expect(kept.textContent).not.toContain('deletes it for good')
      expect(kept.querySelector('[data-testid^="share-settings-"]')).toBeNull()
    }
    // The sheet words what the ticked copies come to, not a plain move.
    const summary = sheetText('keep-summary')
    expect(summary.startsWith('Moves ')).toBe(sheetCopies('moves').every(copy => !copy.querySelector('[data-testid="share-settings-Archive"]')))
    await clickTestId('keep-apply')
    expect(basketGroups().groups.map(group => group.id)).toContain(mockId('render'))
  })

  it('says nothing about deleting for good where every Recycle Bin is on', async () => {
    binsWith()
    const { wrapper } = await mountDuplicates({}, undefined, undefined, true)
    await openShare(wrapper)
    const row = rowOf(wrapper, 'render-final.mov')
    expect(row.text()).not.toContain('Deletes for good')
    expect(row.find('[data-testid="share-settings-Archive"]').exists()).toBe(false)
  })

  it('leaves the Recycle Bin out where DSM does not say', async () => {
    const { wrapper } = await mountDuplicates()
    expect(wrapper.find('[data-testid="duplicates-bin-Archive"]').exists()).toBe(false)
    expect(wrapper.findAll('th').map(th => th.text())).not.toContain('Recycle Bin')
  })

  it('opens a share on its folders in one request, and filters only the large files', async () => {
    const { wrapper, calls } = await mountDuplicates()
    const before = calls.length
    await wrapper.get('[data-testid="duplicates-share-Archive"]').trigger('click')
    await settle()
    expect(calls.slice(before)).toEqual(['duplicates.scope'])
    expect(wrapper.get('[role="tab"][aria-selected="true"]').text()).toMatch(/^Identical folders\s*\d+$/)
    expect(rowOf(wrapper, 'wedding-2022')).toBeDefined()
    expect(wrapper.findAll('[role="radio"], button').some(node => node.text() === 'Videos')).toBe(false)
    await showList(wrapper, 'files')
    expect(calls.slice(before)).toEqual(['duplicates.scope'])
    expect(rowOf(wrapper, 'render-final.mov')).toBeDefined()
    const videos = wrapper.findAll('[role="radio"], button').find(node => node.text() === 'Videos')!
    await videos.trigger('click')
    await settle()
    expect(calls.slice(before)).toEqual(['duplicates.scope', 'duplicates.scope'])
    // Back on the folders the filter goes: it is the large files'.
    await showList(wrapper, 'folders')
    expect(rowOf(wrapper, 'wedding-2022')).toBeDefined()
    expect(wrapper.get('[data-testid="duplicates-select-all"]').text()).toBe('Select all duplicates in Archive')
  })

  it('selects every group of the share under the filter, page by page', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper)
    await wrapper.get('[data-testid="duplicates-select-all"]').trigger('click')
    await settle()
    const ids = basketGroups().groups.map(group => group.id)
    expect(ids).toContain(mockId('render'))
    expect(ids).toContain(mockId('wedding', 'folder'))
    // Nothing to report: the dialog closes by itself.
    expect(byTestId('select-all-dialog')).toBeNull()
  })

  it('leaves a row where it was after its sheet is applied, until the page is left', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper)
    const names = () => wrapper.findAll('tbody tr').filter(tr => tr.find('[role="checkbox"]').exists()).map(tr => tr.text())
    const position = () => names().findIndex(text => text.includes('render-final.mov'))
    const before = position()
    await openGroup(wrapper, 'render-final.mov')
    await tick(sheetCopies('moves')[0]!)
    await clickTestId('keep-apply')
    expect(position()).toBe(before)
  })

  it('greys the numbers, blocks Select all and offers the analysis after preferred shares change', async () => {
    const { wrapper, calls } = await mountDuplicates({ analyzeMs: 60_000 }, mock =>
      mock.answer('duplicates.preferred_shares', { volume: '/volume1', shares: ['Archive'] }))
    expect(wrapper.text()).toContain('Recalculating after a change to preferred shares')
    expect(wrapper.get('.duplicates-scope').classes()).toContain('duplicates-stale')
    expect(wrapper.get('[data-testid="duplicates-select-all"]').attributes('disabled')).toBeDefined()
    const again = wrapper.get('[data-testid="duplicates-recalculating"]').findAll('button').find(node => node.text() === 'Analyze again')!
    await again.trigger('click')
    await settle()
    expect(calls).toContain('duplicates.analyze')
    expect(wrapper.find('[data-testid="duplicates-running"]').exists()).toBe(true)
  })

  it('offers finding duplicates when no analysis of this format exists', async () => {
    const { wrapper, calls } = await mountDuplicates({ analysis: 'none', analyzeMs: 60_000 })
    expect(wrapper.find('[data-testid="duplicates-none"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="duplicates-share-Archive"]').exists()).toBe(false)
    await wrapper.get('[data-testid="duplicates-find"]').trigger('click')
    await settle()
    expect(calls).toContain('duplicates.analyze')
    expect(wrapper.find('[data-testid="duplicates-none"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="duplicates-first-run"]').exists()).toBe(true)
  })

  it('centres the start view and leaves out the scope line and Select all before an analysis', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'none' })
    const start = wrapper.get('[data-testid="duplicates-none"]')
    expect(start.findComponent({ name: 'EmptyState' }).exists()).toBe(true)
    expect(start.classes()).toContain('duplicates-blank')
    expect(wrapper.find('[data-testid="duplicates-scope-title"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="duplicates-select-all"]').exists()).toBe(false)
  })

  it('shows the first analysis as one centred state, without an empty list', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'none', analyzeMs: 60_000 })
    await wrapper.get('[data-testid="duplicates-find"]').trigger('click')
    await settle()
    expect(wrapper.get('[data-testid="duplicates-first-run"]').text()).toContain('Finding duplicates')
    expect(wrapper.find('table').exists()).toBe(false)
  })

  it('says a new scan is needed when finding duplicates fails for lack of scan data', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'none', analyzeMs: 10, analyzeFails: true })
    await wrapper.get('[data-testid="duplicates-find"]').trigger('click')
    await settle()
    await new Promise(resolve => setTimeout(resolve, 1700))
    await settle()
    const unavailable = wrapper.get('[data-testid="duplicates-unavailable"]')
    expect(unavailable.text()).toContain('No duplicate data for this volume')
    expect(unavailable.text()).toContain('Scan')
    expect(wrapper.find('[data-testid="duplicates-none"]').exists()).toBe(false)
  })

  it('shows nothing of its own while the whole volume is scanned', async () => {
    const scan = { kind: 'scan', job_id: 'scan-now', state: 'running', targets: [{ path: '/volume1' }], filesystems: [{ fsid: 'f', mount: { path: '/volume1' } }], progress: { entries: 77645 } }
    const { wrapper } = await mountDuplicates({ analysis: 'none' }, undefined, [scan])
    expect(wrapper.find('[data-testid="duplicates-none"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="duplicates-find"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="duplicates-scope-title"]').exists()).toBe(false)
  })

  it('asks for a scan when the volume has no duplicate data', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'unavailable' })
    expect(wrapper.get('[data-testid="duplicates-unavailable"]').text()).toContain('No duplicate data for this volume')
  })

  it('says why when the answer fails for a reason a scan does not fix', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'unavailable', unavailableMessage: 'Permission denied (os error 13)' })
    expect(wrapper.find('[data-testid="duplicates-unavailable"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('No duplicate data for this volume')
    expect(wrapper.get('[data-testid="duplicates-failed-detail"]').text()).toBe('Permission denied (os error 13)')
  })

  it('offers analyzing again when the results predate the latest scan', async () => {
    const { wrapper } = await mountDuplicates({ analysis: 'stale' })
    expect(wrapper.find('[data-testid="duplicates-stale"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="duplicates-share-Archive"]').exists()).toBe(true)
  })

  it('lists every identical copy in its sheet, those in other shares too', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper)
    expect(rowOf(wrapper, 'tax-scans-2018.zip').text()).toContain('/private/finance')
    await openGroup(wrapper, 'IMG_4471.MOV')
    const listed = byTestId('keep-copies')!.querySelectorAll('li')
    expect(listed).toHaveLength(6)
    expect(byTestId('keep-copies')!.textContent).toContain('/private')
  })

  it('counts a copy inside an identical folder apart, and gives it no box', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper, 'homes')
    expect(rowOf(wrapper, 'vows.mov').text()).toContain('1 here · 1 elsewhere · 1 in identical folders')
    await openGroup(wrapper, 'vows.mov')
    const inFolder = sheetCopies('blocked-inFolder')
    expect(inFolder).toHaveLength(1)
    expect(inFolder[0]!.textContent).toContain('Moves with its identical folder')
    expect(inFolder[0]!.querySelector('[role="checkbox"]')).toBeNull()
  })

  it('ticks every copy but the kept one, whatever its share', async () => {
    const { wrapper } = await mountDuplicates({ preferred: [] })
    await openShare(wrapper)
    expect(rowOf(wrapper, 'ubuntu-24.04.1-desktop-amd64.iso').text()).toContain('6 here · 14 elsewhere')
    await openGroup(wrapper, 'ubuntu-24.04.1-desktop-amd64.iso')
    const kept = sheetCopies('kept')
    expect(kept).toHaveLength(1)
    expect(kept[0]!.textContent).toContain('/software/os/ubuntu')
    expect(sheetCopies('moves').length + sheetCopies('blocked-inFolder').length).toBe(19)
    expect(sheetCopies('moves').some(copy => copy.textContent?.includes('/homes/'))).toBe(true)
  })



  it('merges folders that match the same kept folder into one row, each with its match and date', async () => {
    const { wrapper } = await mountDuplicates()
    await openShare(wrapper, 'homes', 'folders')
    const row = rowOf(wrapper, '2 folders match it: linux-6.6 · linux-6.6-copy-env')
    expect(row.text()).toContain('linux-6.6-copy')
    // What each copy frees stays on one line under the size, so the row keeps its height; the column's tooltip has it whole.
    const canFree = row.findAllComponents(CellText).find((cell: { attributes: (name: string) => string | undefined }) => cell.attributes('data-testid') === 'duplicates-can-free')!
    expect(canFree.props('wrap')).toBe(false)
    await row.get('[role="checkbox"]').trigger('click')
    await settle()
    expect(basketGroups().groups.map(group => group.id).sort()).toEqual([mockId('linux', 'folder_partial'), mockId('linuxenv', 'folder_partial')].sort())
    await openGroup(wrapper, '2 folders match it')
    expect(sheetText('keep-member-kept')).toContain('/Archive/src/linux-6.6-copy')
    const members = [...document.body.querySelectorAll<HTMLElement>('[data-testid="keep-member"]')]
    expect(members.map(member => member.textContent?.replace(/\s+/g, ' '))).toEqual([
      expect.stringContaining('31.9% same · 38 of 39 items'),
      expect.stringContaining('93.5% same · 38 of 40 items'),
    ])
    // Untick one: only the other stays in the cleanup list.
    await tick(members[1]!)
    await clickTestId('keep-apply')
    expect(basketGroups().groups.map(group => group.id)).toEqual([mockId('linux', 'folder_partial')])
  })

  it('pages the identical items when there are more than one page', async () => {
    const { wrapper } = await mountDuplicates({ groups: [{
      key: 'tree', kind: 'folder_partial', name: 'tree', size: 120 * 1024 ** 2, files: 120,
      items: Array.from({ length: 120 }, (_, index) => ({ rel: `file-${String(index).padStart(3, '0')}`, folder: false, files: 1, bytes: (200 - index) * 1024 ** 2 })),
      copies: [
        { at: '/homes/alice/tree', date: '2026-01-01', side: { items: 121, files: 400, bytes: 900 * 1024 ** 2 } },
        { at: '/Archive/tree', date: '2025-01-01', side: { items: 120, files: 120, bytes: 120 * 1024 ** 2 } },
      ],
    }] })
    await openShare(wrapper, 'homes', 'folders')
    await openGroup(wrapper, '120 of 121 items same as /Archive/tree')
    const items = () => document.body.querySelector('[data-testid="partial-items"]')!
    expect(items().textContent).toContain('file-049')
    expect(items().textContent).not.toContain('file-050')
    expect(items().textContent).toContain('70 more identical items')
    ;(items().querySelector('[data-testid="partial-more"]') as HTMLElement).click()
    await settle()
    expect(items().textContent).toContain('file-099')
    expect(items().textContent).toContain('20 more identical items')
  })
})
