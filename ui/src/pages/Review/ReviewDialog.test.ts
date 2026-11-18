import { withPlanningFixtureJobs } from '@/api/planningFixtureJobs'
/* eslint vue/one-component-per-file: off */
import { defineComponent } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import type { Backend } from '@/api/methods'
import { ApiError, setBackend as setPlainBackend } from '@/api'
import { answersVersion } from '@/stores/answers'
import { declineRootWork } from '@/authorizationFixture'
import { useBasket, type DuplicateSelection } from '@/stores/basket'
import { formatDateTime } from '@/i18n'
import { session } from '@/stores/session'
import { openShareSettings } from '@/components/shareSettings'
import ReviewDialog from './ReviewDialog.vue'

vi.mock('@/components/shareSettings', () => ({ openShareSettings: vi.fn(async () => true) }))

const setBackend = (backend: Backend) => setPlainBackend(withPlanningFixtureJobs(backend))

const ConfirmDialogStub = defineComponent({
  props: { showConfirm: Boolean, title: { type: String, default: '' }, description: { type: String, default: undefined }, confirmLabel: { type: String, default: '' } },
  emits: ['confirm', 'update:open', 'close'],
  template: '<div :data-dialog-title="title" :data-dialog-description="description"><slot /><template v-if="showConfirm"><slot name="actions" /><button data-test="review-confirm" @click="$emit(\'confirm\')">{{ confirmLabel }}</button></template><slot v-else name="footer" /></div>',
})
const CalloutStub = defineComponent({
  props: { tone: { type: String, default: 'info' }, title: { type: String, default: '' } },
  template: '<section :data-tone="tone"><strong>{{ title }}</strong><slot /></section>',
})
const ButtonStub = defineComponent({
  props: { variant: { type: String, default: 'secondary' }, disabled: Boolean },
  emits: ['click'],
  template: '<button :disabled="disabled" @click="$emit(\'click\')"><slot /></button>',
})
// The stub renders its slots as ConfirmDialog does: the actions slot beside the
// confirm button, and the footer slot only when there is no confirm button.
const stubs = { ConfirmDialog: ConfirmDialogStub, Callout: CalloutStub, Button: ButtonStub, InfoTip: { template: '<span><slot /></span>' }, Spinner: true }

const jobId = 'cleanup-18d9cccbc2d8b089-12701'
const planId = 'cleanup-18d9cccbc2d8b089-31e7a5'
const MIB = 1048576

/** A shared folder of the plan; one whose Recycle Bin is off is removed for good, as the backend plans it. */
function share(name: string, items: string[], recycle: { enabled: boolean; admin_only: boolean }, drive = false) {
  return {
    share: { path: name },
    path: { path: `/volume1/${name}` },
    recycle_bin: recycle,
    items: items.map(path => ({ path: { path }, kind: 'file', drive, entries: 1, files: 1, allocated_bytes: MIB })),
    entries: items.length,
    files: items.length,
    allocated_bytes: items.length * MIB,
    ...(recycle.enabled ? {} : { delete: true, delete_reason: 'recycle_bin_off' }),
  }
}

function planOf(shares: ReturnType<typeof share>[]) {
  const paths = shares.flatMap(s => s.items.map(item => item.path))
  const deletes = shares.filter(s => s.delete).reduce((sum, s) => sum + s.items.length, 0)
  return {
    command: 'cleanup-plan',
    completion: 'complete',
    errors: [],
    warnings: [],
    plan_id: planId,
    plan_file: { path: `/state/cleanup/plans/${planId}.json` },
    expires_at: '2026-11-14T00:00:00Z',
    items: paths.length,
    deletes,
    paths,
    size: { entries: paths.length, files: paths.length, allocated_bytes: paths.length * MIB },
    shares,
  }
}

const moved = {
  command: 'cleanup-apply',
  completion: 'complete',
  plan_id: planId,
  moved: 1,
  already_absent: 0,
  partial: 0,
  changed: 0,
  failed: 0,
  moved_bytes: MIB,
  targets: [{ path: { path: '/volume1/photos/a.jpg' }, share: { path: 'photos' }, outcome: 'moved', moved_bytes: MIB, remaining_entries: 0, remaining_bytes: 0, remaining: [], detail: null }],
  recycle_bins: [{ share: { path: 'photos' }, path: { path: '/volume1/photos' }, admin_only: true, items: 1, moved_bytes: MIB }],
  report_state_stale: true,
  rescan: [{ path: '/volume1/photos' }],
  space_release: 'freed after emptying',
}

function select(...paths: string[]) {
  const basket = useBasket('/volume1')
  basket.clear()
  basket.clearDuplicates()
  for (const path of paths) basket.add({ path, reclaimable: 512 * 1024 })
}

function backendWith(plan: unknown, apply: () => unknown = () => moved) {
  return vi.fn(async (method: string) => {
    if (method === 'cleanup.plan') return plan
    if (method === 'cleanup.apply') return apply()
    if (method === 'jobs.list') return { jobs: [] }
    if (method === 'simulate.get') throw new ApiError('cli_error')
    return {}
  })
}

describe('ReviewDialog', () => {
  beforeEach(() => {
    sessionStorage.clear()
    select('/volume1/photos/a.jpg')
    session.cleanupEnabled = true
    session.mutationsDisabled = false
  })

  it('moves the plan to the Recycle Bin and says the space is freed after emptying it', async () => {
    const backend = backendWith(planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    const description = wrapper.get('[data-dialog-description]').attributes('data-dialog-description')
    expect(description).toBe('Will move to Recycle Bin: 1 item, 1 MB; empty the share\'s Recycle Bin to free about 512 KB')
    expect(wrapper.text()).toContain('Shared folder photos')
    expect(wrapper.get('[data-test="review-admin-only"]').text()).toContain('Only administrators')
    expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Move 1 item to Recycle Bin')
    const before = answersVersion.value
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(backend).toHaveBeenCalledWith('cleanup.apply', { plan_id: planId, confirm_move: true })
    expect(answersVersion.value).toBe(before + 1)
    expect(wrapper.get('[data-test="review-moved"]').text()).toContain('Moved to Recycle Bin: 1 MB; space is freed after emptying it')
    expect(useBasket('/volume1').count.value).toBe(0)
    wrapper.unmount()
  })

  it('plans afresh when opened again after a finished move, as an always-mounted review is', async () => {
    const backend = backendWith(planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-test="review-moved"]').exists()).toBe(true)
    await wrapper.setProps({ open: false })
    select('/volume1/photos/b.jpg')
    await wrapper.setProps({ open: true })
    await flushPromises()
    expect(wrapper.find('[data-test="review-moved"]').exists()).toBe(false)
    expect(backend.mock.calls.filter(([method]) => method === 'cleanup.plan')).toHaveLength(2)
    expect(wrapper.find('[data-test="review-confirm"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('deletes the items of a shared folder whose Recycle Bin is off for good, saying why and where to turn it on', async () => {
    select('/volume1/video/a.mkv')
    const backend = backendWith(planOf([share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false })]), () => ({ ...moved, moved: 0, moved_bytes: 0, deleted: 1, deleted_bytes: MIB, items: 1, targets: [], recycle_bins: [] }))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    const section = wrapper.get('[data-test="review-share-delete"]')
    // A shared folder still: its name, not "not a shared folder".
    expect(section.text()).toContain('Shared folder video')
    expect(section.text()).not.toContain('not a shared folder')
    const why = wrapper.get('[data-test="review-delete-bin-off"]')
    expect(why.text()).toContain('video has no Recycle Bin turned on: its items are deleted for good')
    expect(why.text()).toContain('turn its Recycle Bin on first, then select Recheck')
    expect(wrapper.find('[data-test="review-delete-share"]').exists()).toBe(false)
    expect(wrapper.get('[data-test="review-delete"]').text()).toContain('1 item is deleted for good')
    expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Delete 1 item for good')
    expect(wrapper.getComponent(ConfirmDialogStub).attributes('check-label')).toBe('I understand 1 item is deleted for good and cannot be restored')
    // Turning the Recycle Bin on and checking again moves them instead.
    expect(wrapper.find('[data-test="review-recheck"]').exists()).toBe(true)
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(backend).toHaveBeenCalledWith('cleanup.apply', { plan_id: planId, confirm_move: true, confirm_delete: true })
    expect(wrapper.get('[data-test="review-deleted"]').text()).toContain('Deleted for good: 1 item, 1 MB')
    // Nothing went to a Recycle Bin: the result says deleted, with no restore from #recycle.
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Deleted for good')
    expect(wrapper.find('[data-test="review-moved"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('#recycle')
    // What was deleted leaves the cleanup list, as what moved does.
    expect(useBasket('/volume1').count.value).toBe(0)
    wrapper.unmount()
  })

  it('shows Recheck beside the confirm button of the real ConfirmDialog when a Recycle Bin is off', async () => {
    select('/volume1/video/a.mkv')
    const backend = backendWith(planOf([share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false })]))
    setBackend(backend as Backend)
    const real = {
      ...stubs,
      ConfirmDialog: false,
      Dialog: { template: '<div><slot /><slot name="footer" /></div>' },
      Checkbox: { props: ['label'], template: '<label>{{ label }}</label>' },
    }
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs: real } })
    await flushPromises()
    const footer = wrapper.get('.confirm-dialog__actions')
    expect(footer.text()).toContain('Delete 1 item for good')
    const plans = backend.mock.calls.filter(([method]) => method === 'cleanup.plan').length
    await footer.get('[data-test="review-recheck"]').trigger('click')
    await flushPromises()
    expect(backend.mock.calls.filter(([method]) => method === 'cleanup.plan').length).toBe(plans + 1)
    wrapper.unmount()
  })

  it('moves the shared folders that have a Recycle Bin and deletes the others for good in one go', async () => {
    select('/volume1/photos/a.jpg', '/volume1/video/a.mkv')
    const backend = backendWith(planOf([
      share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: false }, true),
      share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false }),
    ]))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Move 1, delete 1 for good')
    expect(wrapper.get('[data-test="review-everyone"]').text()).toContain('Everyone who can open photos')
    expect(wrapper.find('[data-test="review-drive-item"]').exists()).toBe(true)
    expect(wrapper.find('[data-test="review-drive"]').exists()).toBe(true)
    expect(wrapper.find('[data-test="review-delete-bin-off"]').exists()).toBe(true)
    expect(wrapper.getComponent(ConfirmDialogStub).attributes('check-label')).toBe('I understand 1 item is deleted for good and cannot be restored; the others move to the Recycle Bin')
    wrapper.unmount()
  })

  it('refuses an item in a Recycle Bin and lets it be removed from the list', async () => {
    select('/volume1/photos/#recycle/old.jpg')
    const backend = backendWith({ command: 'cleanup-plan', completion: 'failed', errors: [{ path: '/volume1/photos/#recycle/old.jpg', code: 'in_recycle_bin', message: 'in bin' }], warnings: [] })
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.text()).toContain('Already in a Recycle Bin')
    await wrapper.get('[data-test="drop-changed"]').trigger('click')
    expect(useBasket('/volume1').count.value).toBe(0)
    wrapper.unmount()
  })

  it('lists what DSM left in place after a partial move', async () => {
    const partial = { ...moved, completion: 'partial', moved: 0, partial: 1, moved_bytes: 0, recycle_bins: [], targets: [{ ...moved.targets[0], outcome: 'partial', moved_bytes: 0, remaining_entries: 1, remaining: [{ path: '/volume1/photos/a.jpg' }], detail: 'exited 255' }] }
    setBackend(backendWith(planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]), () => partial) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Some items stayed in place')
    expect(wrapper.get('[data-test="review-left"]').text()).toContain('/volume1/photos/a.jpg')
    expect(wrapper.get('[data-test="review-left"]').text()).toContain('Partly moved')
    expect(useBasket('/volume1').count.value).toBe(1)
    wrapper.unmount()
  })

  it('says a duplicate group whose copies have no Recycle Bin is deleted for good, not moved', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    basket.addDuplicates([{ id: 'c'.repeat(32), name: 'linux', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/archive/linux', preferred: false, drive: false, shares: ['media'] }])
    const backend = backendWith({ ...planOf([share('media', ['/volume1/media/linux'], { enabled: false, admin_only: false })]), duplicates: { copies: 1, groups: 1, kept: 1, outside_shares: 0, left: [], scan_finished: 1_762_934_400 } })
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    const section = wrapper.get('[data-test="review-duplicates"]')
    expect(section.text()).toContain('Deletes 1 for good · keeps /volume1/archive/linux')
    expect(section.text()).not.toContain('Recycle Bin · keeps')
    expect(wrapper.get('[data-test="review-duplicates-scan-age"]').text()).toContain('to the Recycle Bin, or for good where there is none')
    expect(wrapper.get('[data-test="review-kept-checked"]').text()).toBe('1 kept copy is checked again just before anything is removed')
    wrapper.unmount()
    basket.clear()
  })

  it('stops before anything moves when a kept duplicate went', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    const group: DuplicateSelection = { id: 'c'.repeat(32), name: 'd.iso', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/software/d.iso', preferred: false, drive: false }
    basket.addDuplicates([group])
    const backend = backendWith(
      { ...planOf([share('Archive', ['/volume1/Archive/d.iso'], { enabled: true, admin_only: true })]), duplicates: { copies: 1, groups: 1, kept: 1, outside_shares: 0, left: [], scan_finished: 1_762_934_400 } },
      () => ({ command: 'cleanup-apply', completion: 'failed', errors: [{ code: 'keeper_changed', path: '/volume1/software/d.iso' }] }),
    )
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(backend).toHaveBeenCalledWith('cleanup.plan', { paths: [], duplicates: { volume: '/volume1', groups: ['c'.repeat(32)] } })
    expect(wrapper.get('[data-test="review-duplicates"]').text()).toContain('Moves 1 to Recycle Bin · keeps /volume1/software/d.iso')
    // The copies moved are as the scan saw them: what changed in them since moves too, restorable.
    expect(wrapper.get('[data-test="review-duplicates-scan-age"]').text()).toBe(`These results come from the scan of ${formatDateTime('2025-11-12T08:00:00.000Z')}. Files added to or changed in the copies being moved since then go to the Recycle Bin too, where they can be restored. If those folders may have changed, scan again first`)
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('A copy that was to be kept changed or went missing')
    expect(wrapper.find('[data-test="review-rescan"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('refuses a plan that would remove a copy kept in another group, and offers to take the groups out', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    basket.addDuplicates([{ id: 'c'.repeat(32), name: 'linux-6.6-copy-junk', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/archive/src/linux-6.6-copy-junk', preferred: false, drive: false }])
    setBackend(backendWith({ command: 'cleanup-plan', completion: 'failed', errors: [{ code: 'keeper_conflict', path: '/volume1', message: 'keeper_conflict: …' }] }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.text()).toContain('A selected group would remove a copy you chose to keep in another group, so nothing moved')
    // Without the groups the CLI names, only taking them all out is left.
    expect(wrapper.find('[data-test="review-clear-duplicates"]').exists()).toBe(true)
    expect(wrapper.find('[data-test="review-resolve-conflicts"]').exists()).toBe(false)
    wrapper.unmount()
    basket.clear()
  })

  it('names the kept copy and both groups of a keeper conflict', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    basket.addDuplicates([{ id: 'c'.repeat(32), name: 'linux-6.6-copy-junk', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/archive/src/linux-6.6-copy-junk', preferred: false, drive: false }])
    const conflict = { kept_in: { id: 'e'.repeat(32), name: { path: 'linux-6.6-copy-env' } }, removed_by: { id: 'c'.repeat(32), name: { path: 'linux-6.6-copy-junk' } } }
    setBackend(backendWith({ command: 'cleanup-plan', completion: 'failed', errors: [{ code: 'keeper_conflict', path: '/volume1/media/src/linux-6.6-copy', message: 'keeper_conflict: …', conflict }] }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.text()).toContain('/volume1/media/src/linux-6.6-copy is kept in “linux-6.6-copy-env”, but removing “linux-6.6-copy-junk” would remove it, so nothing moved')
    expect(wrapper.text()).not.toContain('keeper_conflict:')
    expect(wrapper.find('[data-test="review-resolve-conflicts"]').text()).toBe('Remove linux-6.6-copy-junk from the list')
    expect(wrapper.find('[data-test="review-clear-duplicates"]').exists()).toBe(false)
    wrapper.unmount()
    basket.clear()
  })

  describe('settling keeper conflicts', () => {
    const MEDIA = '/volume1/media/src/linux-6.6-copy'
    const group = (id: string, name: string): DuplicateSelection => ({ id, name, copies: 1, canFree: MIB, freesLater: 0, keeps: `/volume1/archive/src/${name}`, preferred: false, drive: false })
    const ENV = group('e'.repeat(32), 'linux-6.6-copy-env')
    const JUNK = group('c'.repeat(32), 'linux-6.6-copy-junk')
    const SCSI = group('5'.repeat(32), 'scsi')
    const CLK = group('6'.repeat(32), 'clk')
    const ref = (g: DuplicateSelection) => ({ id: g.id, name: { path: g.name } })
    const conflictError = (kept: DuplicateSelection, removing: DuplicateSelection) => ({ code: 'keeper_conflict', path: MEDIA, message: 'keeper_conflict: …', conflict: { kept_in: ref(kept), removed_by: ref(removing) } })
    const ready = { ...planOf([share('archive', ['/volume1/archive/src/linux-6.6-copy-env'], { enabled: true, admin_only: true })]), duplicates: { copies: 1, groups: 1, kept: 1, outside_shares: 0, left: [], scan_finished: 1_762_934_400 } }

    /** A backend whose plan answers by the groups it is sent. */
    function planBy(answer: (groups: string[]) => unknown) {
      const plans: string[][] = []
      const backend = vi.fn(async (method: string, params?: { duplicates?: { groups: string[] } }) => {
        if (method === 'cleanup.plan') {
          const groups = params?.duplicates?.groups ?? []
          plans.push(groups)
          return answer(groups)
        }
        if (method === 'jobs.list') return { jobs: [] }
        if (method === 'simulate.get') throw new ApiError('cli_error')
        return {}
      })
      setBackend(backend as unknown as Backend)
      return plans
    }
    function listOf(...groups: DuplicateSelection[]) {
      const basket = useBasket('/volume1')
      basket.clear()
      basket.addDuplicates(groups)
      return basket
    }
    const failed = (...errors: unknown[]) => ({ command: 'cleanup-plan', completion: 'failed', errors })

    it('takes out only the group that removes the kept copy, checks again and stays on the list', async () => {
      const basket = listOf(ENV, JUNK)
      const plans = planBy(groups => groups.includes(JUNK.id) ? failed(conflictError(ENV, JUNK)) : ready)
      const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
      await flushPromises()
      await wrapper.find('[data-test="review-resolve-conflicts"]').trigger('click')
      await flushPromises()
      await flushPromises()
      expect(basket.duplicates.value.groups.map(g => g.id)).toEqual([ENV.id])
      expect(plans).toEqual([[ENV.id, JUNK.id], [ENV.id]])
      expect(wrapper.emitted('update:open')).toBeUndefined()
      expect(wrapper.find('[data-test="review-confirm"]').exists()).toBe(true)
      expect(wrapper.find('[data-test="review-dropped-conflicts"]').text()).toContain('linux-6.6-copy-junk')
      wrapper.unmount()
      basket.clear()
    })

    it('lists every conflict the plan names and takes all their groups out in one go', async () => {
      const basket = listOf(ENV, JUNK, SCSI, CLK)
      const plans = planBy(groups => groups.includes(JUNK.id) ? failed(conflictError(ENV, JUNK), conflictError(ENV, SCSI)) : ready)
      const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
      await flushPromises()
      expect(wrapper.text()).toContain('2 selected groups would remove copies you chose to keep, so nothing moved')
      expect(wrapper.findAll('[data-test="review-conflict"]').map(line => line.text())).toEqual([
        `Removing “linux-6.6-copy-junk” would remove ${MEDIA}, kept in “linux-6.6-copy-env”`,
        `Removing “scsi” would remove ${MEDIA}, kept in “linux-6.6-copy-env”`,
      ])
      const button = wrapper.find('[data-test="review-resolve-conflicts"]')
      expect(button.text()).toBe('Remove linux-6.6-copy-junk and 1 more group from the list')
      expect(button.text()).not.toMatch(/[.…]$/)
      await button.trigger('click')
      await flushPromises()
      await flushPromises()
      expect(basket.duplicates.value.groups.map(g => g.id)).toEqual([ENV.id, CLK.id])
      expect(plans).toHaveLength(2)
      expect(wrapper.find('[data-test="review-confirm"]').exists()).toBe(true)
      wrapper.unmount()
      basket.clear()
    })

    it('keeps settling when a CLI names one conflict at a time, without stopping in between', async () => {
      const basket = listOf(ENV, JUNK, SCSI)
      const plans = planBy(groups => groups.includes(JUNK.id)
        ? failed(conflictError(ENV, JUNK))
        : groups.includes(SCSI.id) ? failed(conflictError(ENV, SCSI)) : ready)
      const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
      await flushPromises()
      await wrapper.find('[data-test="review-resolve-conflicts"]').trigger('click')
      for (let i = 0; i < 4; i++) await flushPromises()
      expect(plans).toEqual([[ENV.id, JUNK.id, SCSI.id], [ENV.id, SCSI.id], [ENV.id]])
      expect(wrapper.find('[data-test="review-confirm"]').exists()).toBe(true)
      expect(wrapper.find('[data-test="review-dropped-conflicts"]').text()).toContain('scsi')
      wrapper.unmount()
      basket.clear()
    })

    it('asks in the dialog which of two groups to take out when each removes the other\'s kept copy', async () => {
      const basket = listOf(ENV, JUNK)
      planBy(groups => groups.length === 2 ? failed(conflictError(ENV, JUNK), conflictError(JUNK, ENV)) : ready)
      const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
      await flushPromises()
      const resolve = () => wrapper.find('[data-test="review-resolve-conflicts"]')
      expect(resolve().attributes('disabled')).toBeDefined()
      expect(wrapper.find('[data-test="review-conflict-choice"]').text()).toContain('“linux-6.6-copy-env” and “linux-6.6-copy-junk” each remove the copy kept in the other')
      const picks = wrapper.findAll('[data-test="review-conflict-pick"]')
      expect(picks.map(pick => pick.text())).toEqual(['Remove linux-6.6-copy-env from the list', 'Remove linux-6.6-copy-junk from the list'])
      await picks[1].trigger('click')
      expect(resolve().attributes('disabled')).toBeUndefined()
      expect(resolve().text()).toBe('Remove linux-6.6-copy-junk from the list')
      await resolve().trigger('click')
      await flushPromises()
      await flushPromises()
      expect(basket.duplicates.value.groups.map(g => g.id)).toEqual([ENV.id])
      expect(wrapper.find('[data-test="review-confirm"]').exists()).toBe(true)
      wrapper.unmount()
      basket.clear()
    })
  })

  it('follows a running move and its counts without sending it again', async () => {
    vi.useFakeTimers()
    const running = {
      job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'running',
      cleanup: { plan_id: planId, items_total: 4, items_done: 2, moved: 1, already_absent: 1, partial: 0, changed: 0, failed: 0, bytes_total: 4 * MIB, bytes_moved: MIB, elapsed_s: 3, updated: '2026-11-13T00:00:00Z' },
    }
    const calls: string[] = []
    setBackend((async method => {
      calls.push(method)
      if (method === 'jobs.list') return { jobs: [running] } as never
      return {} as never
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, jobId }, global: { stubs } })
    try {
      await flushPromises()
      expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Moving to Recycle Bin')
      expect(wrapper.text()).toContain('2 / 4 items')
      expect(wrapper.text()).toContain('1 moved · 1 already gone')
      expect(calls).not.toContain('cleanup.apply')
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('counts a running delete-only cleanup as deleted for good, not as 0 moved', async () => {
    vi.useFakeTimers()
    const running = {
      job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'running',
      cleanup: { plan_id: planId, items_total: 4, items_done: 2, moved: 0, deleted: 2, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: 4 * MIB, bytes_moved: 2 * MIB, elapsed_s: 3, updated: '2026-11-13T00:00:00Z' },
    }
    setBackend((async method => (method === 'jobs.list' ? { jobs: [running] } : {}) as never) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, jobId }, global: { stubs } })
    try {
      await flushPromises()
      expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Deleting for good')
      expect(wrapper.get('[data-test="review-applying-counts"]').text()).toBe('2 deleted for good')
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('says 0 deleted for good, not 0 moved, before a delete-only cleanup removes anything', async () => {
    select('/volume1/video/a.mkv')
    let answer: (value: unknown) => void = () => {}
    const progress = { plan_id: planId, items_total: 1, items_done: 0, moved: 0, deleted: 0, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: MIB, bytes_moved: 0, elapsed_s: 0, updated: '2026-11-13T00:00:00Z' }
    let named = false
    setBackend((async (method: string) => {
      if (method === 'cleanup.plan') return planOf([share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false })]) as never
      if (method === 'cleanup.apply') return new Promise(resolve => { answer = value => { named = true; resolve(value) } }) as never
      if (method === 'jobs.list') return { jobs: named ? [{ job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'running', cleanup: progress }] : [] } as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    answer({ command: 'detach', job_id: jobId })
    await flushPromises()
    expect(wrapper.get('[data-test="review-applying-counts"]').text()).toBe('0 deleted for good')
    wrapper.unmount()
  })

  it('says the kept copies are being checked while a duplicates cleanup is queued', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    basket.addDuplicates([{ id: 'c'.repeat(32), name: 'd.iso', copies: 2, canFree: MIB, freesLater: 0, keeps: '/volume1/software/d.iso', preferred: false, drive: false }])
    let answer: (value: unknown) => void = () => {}
    const backend = backendWith(
      { ...planOf([share('Archive', ['/volume1/Archive/d.iso'], { enabled: true, admin_only: true })]), duplicates: { copies: 2, groups: 2, kept: 2, outside_shares: 0, left: [], scan_finished: 1_762_934_400 } },
      () => new Promise(resolve => { answer = resolve }),
    )
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Checking 2 kept copies')
    expect(wrapper.text()).not.toContain('Queued')
    answer({ command: 'detach', job_id: jobId })
    await flushPromises()
    wrapper.unmount()
    basket.clear()
  })

  it('says the current item finishes before a cancelled cleanup stops', async () => {
    vi.useFakeTimers()
    let state = 'running'
    const running = () => ({
      job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state,
      cleanup: { plan_id: planId, items_total: 4, items_done: 1, moved: 1, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: 4 * MIB, bytes_moved: MIB, elapsed_s: 3, updated: '2026-11-13T00:00:00Z' },
    })
    setBackend((async method => {
      if (method === 'jobs.list') return { jobs: [running()] } as never
      if (method === 'job.cancel') { state = 'cancelling'; return { outcome: 'requested', state } as never }
      return {} as never
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, jobId }, global: { stubs } })
    try {
      await flushPromises()
      expect(wrapper.find('[data-test="review-finishing"]').exists()).toBe(false)
      await wrapper.get('[data-test="review-cancel"]').trigger('click')
      await flushPromises()
      expect(wrapper.get('[data-test="review-finishing"]').text()).toBe('Finishing the current item, then stopping; the rest stays where it is')
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('finds the job when the apply answer is lost, and never applies twice', async () => {
    let sent = false
    const calls: string[] = []
    setBackend((async method => {
      calls.push(method)
      if (method === 'cleanup.plan') return planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]) as never
      if (method === 'cleanup.apply') { sent = true; throw new ApiError('timeout') }
      if (method === 'jobs.list') return { jobs: sent ? [{ job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'completed', plan_id: planId, created: Date.now() / 1000 }] : [] } as never
      if (method === 'job.result') return moved as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(calls.filter(method => method === 'cleanup.apply')).toHaveLength(1)
    wrapper.unmount()
  })

  it('stages a large cleanup list, shows the plan check progress, and previews a plan of any size', async () => {
    vi.useFakeTimers()
    const paths = Array.from({ length: 2000 }, (_, i) => `/volume1/photos/2026/${'folder/'.repeat(6)}IMG_${String(i).padStart(5, '0')}.jpg`)
    select(...paths)
    const big = share('photos', paths.slice(0, 100), { enabled: true, admin_only: true })
    const plan = { ...planOf([big]), items: 2000, size: { entries: 2000, files: 2000, allocated_bytes: 2000 * MIB }, shares: [{ ...big, item_count: 2000, drive_items: 0, entries: 2000, files: 2000, allocated_bytes: 2000 * MIB }] }
    const calls: [string, Record<string, unknown>][] = []
    let polls = 0
    setPlainBackend((async (method: string, params: Record<string, unknown>) => {
      calls.push([method, params])
      if (method === 'cleanup.select') return { command: 'cleanup-select', completion: 'complete', selection: 'selection-18e-4f', paths: 0, groups: 0 } as never
      if (method === 'cleanup.plan') return { command: 'detach', job_id: 'plan-1' } as never
      if (method === 'jobs.list') {
        polls += 1
        return { jobs: [{ job_id: 'plan-1', kind: 'cleanup-plan', phase: 'plan', state: polls < 2 ? 'running' : 'completed', progress: { units_done: 1200, units_total: 2000 } }] } as never
      }
      if (method === 'job.result') return plan as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    try {
      await flushPromises()
      expect(wrapper.text()).toContain('Checking the selected items: 1,200 / 2,000')
      await vi.advanceTimersByTimeAsync(1100)
      await flushPromises()
      const staged = calls.filter(([method]) => method === 'cleanup.select')
      expect(staged.length).toBeGreaterThan(1)
      expect(staged.flatMap(([, params]) => (params.paths as { path: string }[]) ?? [])).toHaveLength(2000)
      expect(calls.find(([method]) => method === 'cleanup.plan')?.[1]).toEqual({ selection: 'selection-18e-4f' })
      expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Move 2,000 items to Recycle Bin')
      expect(wrapper.get('[data-test="review-count"]').text()).toBe('2,000')
      expect(wrapper.findAll('[data-test="review-share"] li')).toHaveLength(100)
      expect(wrapper.get('[data-test="review-more"]').text()).toBe('And 1,900 more')
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('says precisely why a protected item is refused, never the old catch-all', async () => {
    select('/volume1/restore-test/package-meta.tar', '/volume1/@appstore/x', '/volume1/photos')
    const errors = [
      { path: '/volume1/restore-test/package-meta.tar', code: 'protected_target', reason: 'outside_share', message: 'not inside a DSM shared folder' },
      { path: '/volume1/@appstore/x', code: 'protected_target', reason: 'package', package: 'Hyper Backup', message: 'package data' },
      { path: '/volume1/photos', code: 'protected_target', reason: 'share_root', message: 'shared-folder root' },
    ]
    setBackend(backendWith({ command: 'cleanup-plan', completion: 'failed', errors, warnings: [] }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    const items = wrapper.findAll('[data-test="review-refused-item"]').map(item => item.text())
    expect(items[0]).toContain('Not in a DSM shared folder: cleanup moves items only into a shared folder\'s Recycle Bin')
    expect(items[1]).toContain('Data of Hyper Backup: free its space in Hyper Backup')
    expect(items[2]).toContain('A shared folder itself: select items inside it')
    expect(wrapper.text()).not.toContain('or is a shared folder itself')
    // Each refused item can still be taken out of the list.
    expect(wrapper.findAll('[data-test="drop-changed"]')).toHaveLength(3)
    wrapper.unmount()
  })

  it('names a refused duplicates selection instead of the catch-all', async () => {
    select('/volume1/video/a.mkv')
    const errors = [{ path: '/volume1/video/a.mkv', code: 'duplicates_invalid', message: 'duplicates_invalid: selection' }]
    setBackend(backendWith({ command: 'cleanup-plan', completion: 'failed', errors, warnings: [] }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs: { ...stubs, Callout: { props: ['title'], template: '<section>{{ title }}</section>' } } } })
    await flushPromises()
    expect(wrapper.text()).toContain('The duplicate results are older than the latest scan')
    expect(wrapper.text()).not.toContain('Cleanup was refused. Recheck the selected items')
    wrapper.unmount()
  })

  it('opens the Control Panel settings of a shared folder without a Recycle Bin', async () => {
    select('/volume1/video/a.mkv')
    setBackend(backendWith(planOf([share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false })])) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs: { ...stubs, Button: false, Callout: { template: '<section><slot /><slot name="actions" /></section>' } } } })
    await flushPromises()
    const button = wrapper.get('[data-test="review-share-delete"] [data-testid="share-settings-video"]')
    expect(button.text()).toBe('Shared folder settings')
    await button.trigger('click')
    expect(openShareSettings).toHaveBeenCalledWith('video')
    wrapper.unmount()
  })

  it('deletes items in no shared folder for good only after saying so, and sends that confirmation', async () => {
    select('/volume1/photos/a.jpg', '/volume1/restore-test/package-meta.tar')
    const outside = { ...share('restore-test', ['/volume1/restore-test/package-meta.tar'], { enabled: false, admin_only: false }), delete: true, delete_reason: 'no_shared_folder' }
    const plan = { ...planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]), shares: [share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true }), outside], items: 2, deletes: 1 }
    const backend = backendWith(plan, () => ({ ...moved, deleted: 1, deleted_bytes: MIB, items: 2 }))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.get('[data-test="review-delete"]').text()).toContain('1 item is deleted for good')
    expect(wrapper.get('[data-test="review-delete"]').text()).toContain('cannot be restored')
    expect(wrapper.get('[data-test="review-delete-share"]').text()).toContain('Not in a DSM shared folder: the items in restore-test are deleted for good')
    expect(wrapper.get('[data-test="review-share-delete"]').text()).toContain('Folder restore-test (not a shared folder)')
    // Not a share whose Recycle Bin is off: no settings button, no Recheck.
    expect(wrapper.find('[data-test="review-delete-bin-off"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="share-settings-restore-test"]').exists()).toBe(false)
    expect(wrapper.find('[data-test="review-recheck"]').exists()).toBe(false)
    expect(wrapper.get('[data-dialog-description]').attributes('data-dialog-description')).toBe('Will move to Recycle Bin: 1 item, 1 MB · 1 item is deleted for good')
    expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Move 1, delete 1 for good')
    expect(wrapper.getComponent(ConfirmDialogStub).attributes('check-label')).toBe('I understand 1 item is deleted for good and cannot be restored; the others move to the Recycle Bin')
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(backend).toHaveBeenCalledWith('cleanup.apply', { plan_id: planId, confirm_move: true, confirm_delete: true })
    expect(wrapper.get('[data-test="review-deleted"]').text()).toContain('Deleted for good: 1 item, 1 MB')
    wrapper.unmount()
  })

  it('says delete for good on the button when nothing else moves, and names an unconfirmed delete', async () => {
    select('/volume1/restore-test/package-meta.tar')
    const outside = { ...share('restore-test', ['/volume1/restore-test/package-meta.tar'], { enabled: false, admin_only: false }), delete: true, delete_reason: 'no_shared_folder' }
    const plan = { ...planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]), shares: [outside], paths: [{ path: '/volume1/restore-test/package-meta.tar' }], items: 1, deletes: 1 }
    setBackend(backendWith(plan, () => ({ command: 'cleanup-apply', completion: 'failed', errors: [{ code: 'delete_not_confirmed' }] })) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.find('[data-test="review-recycle"]').exists()).toBe(false)
    expect(wrapper.get('[data-test="review-confirm"]').text()).toBe('Delete 1 item for good')
    // Nothing else moves, so the box does not mention other items.
    expect(wrapper.getComponent(ConfirmDialogStub).attributes('check-label')).toBe('I understand 1 item is deleted for good and cannot be restored')
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('This plan deletes items with no Recycle Bin for good: confirm that to go on')
    wrapper.unmount()
  })

  it('shows the plan check step with its count, and Stop check cancels the job without a failure', async () => {
    vi.useFakeTimers()
    const basket = useBasket('/volume1')
    basket.clear()
    basket.addDuplicates([{ id: 'c'.repeat(32), name: 'd.iso', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/software/d.iso', preferred: false, drive: false }])
    const calls: [string, Record<string, unknown>][] = []
    setPlainBackend((async (method: string, params: Record<string, unknown>) => {
      calls.push([method, params])
      if (method === 'cleanup.plan') return { command: 'detach', job_id: 'plan-2' } as never
      if (method === 'jobs.list') return { jobs: [{ job_id: 'plan-2', kind: 'cleanup-plan', phase: 'plan', state: 'running', progress: { units_done: 40, units_total: 300, step: 'kept' } }] } as never
      if (method === 'job.cancel') return { command: 'cancel', outcome: 'requested', state: 'cancelling', job_id: 'plan-2' } as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    try {
      await flushPromises()
      expect(wrapper.text()).toContain('Checking the copies kept: 40 / 300')
      await wrapper.get('[data-test="review-stop-plan"]').trigger('click')
      await flushPromises()
      expect(calls).toContainEqual(['job.cancel', { job_id: 'plan-2' }])
      expect(wrapper.get('[data-test="review-stopped"]').text()).toContain('Check stopped')
      expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).not.toBe('Unable to start')
      expect(wrapper.find('[data-test="review-recheck"]').exists()).toBe(true)
      // The dialog no longer follows the job: it stays stopped while the job winds down.
      await vi.advanceTimersByTimeAsync(3000)
      await flushPromises()
      expect(wrapper.find('[data-test="review-stopped"]').exists()).toBe(true)
      expect(wrapper.find('[data-test="review-planning"]').exists()).toBe(false)
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('sends no apply when the authorization dialog is dismissed', async () => {
    const backend = backendWith(planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })]))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    const undo = declineRootWork()
    try {
      await wrapper.get('[data-test="review-confirm"]').trigger('click')
      await flushPromises()
      expect(backend.mock.calls.map(([method]) => method)).not.toContain('cleanup.apply')
    } finally {
      undo()
      wrapper.unmount()
    }
  })
  it('says deleting for good while a delete-only cleanup runs, without the Recycle Bin note', async () => {
    select('/volume1/video/a.mkv')
    let answer: (value: unknown) => void = () => {}
    const backend = backendWith(planOf([share('video', ['/volume1/video/a.mkv'], { enabled: false, admin_only: false })]), () => new Promise(resolve => { answer = resolve }))
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Deleting for good')
    expect(wrapper.text()).not.toContain('Big moves take time')
    // Cancel is there while the job is still being queued.
    expect(wrapper.find('[data-test="review-cancel"]').exists()).toBe(true)
    answer({ command: 'detach', job_id: jobId })
    await flushPromises()
    wrapper.unmount()
  })

  it('remembers a cancel pressed while the job is queued and sends it once the job is named', async () => {
    let answer: (value: unknown) => void = () => {}
    const calls: [string, unknown][] = []
    const progress = { plan_id: planId, items_total: 2, items_done: 0, moved: 0, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: 2 * MIB, bytes_moved: 0, elapsed_s: 0, updated: '2026-11-13T00:00:00Z' }
    setBackend((async (method: string, params: unknown) => {
      calls.push([method, params])
      if (method === 'cleanup.plan') return planOf([share('photos', ['/volume1/photos/a.jpg', '/volume1/photos/b.jpg'], { enabled: true, admin_only: true })]) as never
      if (method === 'cleanup.apply') return new Promise(resolve => { answer = resolve }) as never
      if (method === 'job.cancel') return { outcome: 'cancelled', state: 'cancelled' } as never
      if (method === 'jobs.list') return { jobs: calls.some(([name]) => name === 'job.cancel') ? [{ job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'cancelled', cleanup: progress }] : [] } as never
      throw new ApiError('cli_error')
    }) as Backend)
    select('/volume1/photos/a.jpg', '/volume1/photos/b.jpg')
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    await wrapper.get('[data-test="review-confirm"]').trigger('click')
    await flushPromises()
    await wrapper.get('[data-test="review-cancel"]').trigger('click')
    await flushPromises()
    expect(calls.map(([method]) => method)).not.toContain('job.cancel')
    answer({ command: 'detach', job_id: jobId })
    await flushPromises()
    expect(calls).toContainEqual(['job.cancel', { job_id: jobId }])
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Cleanup cancelled')
    expect(wrapper.get('[data-test="review-cancelled-counts"]').text()).toBe('Cancelled: 0 done, 2 items not handled and left as they were')
    wrapper.unmount()
  })

  it('says what a cancelled move did and left after the current item', async () => {
    const result = { ...moved, completion: 'cancelled', items: 3, moved: 1, moved_bytes: MIB }
    setBackend((async (method: string) => {
      if (method === 'jobs.list') return { jobs: [{ job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state: 'cancelled' }] } as never
      if (method === 'job.result') return result as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, jobId }, global: { stubs } })
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Cleanup cancelled')
    expect(wrapper.get('[data-test="review-cancelled-counts"]').text()).toBe('Cancelled: 1 done, 2 items not handled and left as they were')
    wrapper.unmount()
  })

  it('follows the job again when reopened after closing while it ran', async () => {
    let state = 'running'
    setBackend((async (method: string) => {
      const done = state === 'running' ? 0 : 1
      if (method === 'jobs.list') return { jobs: [{ job_id: jobId, kind: 'cleanup-apply', phase: 'execute', state, cleanup: { plan_id: planId, items_total: 1, items_done: done, moved: done, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: MIB, bytes_moved: 0, elapsed_s: 1, updated: '2026-11-13T00:00:00Z' } }] } as never
      if (method === 'job.result') return moved as never
      throw new ApiError('cli_error')
    }) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, jobId }, global: { stubs } })
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Moving to Recycle Bin')
    await wrapper.setProps({ open: false })
    await flushPromises()
    state = 'completed'
    await wrapper.setProps({ open: true })
    await flushPromises()
    expect(wrapper.get('[data-dialog-title]').attributes('data-dialog-title')).toBe('Moved to Recycle Bin')
    wrapper.unmount()
  })

  it('says an item that is no longer there is gone and lets it be removed from the list', async () => {
    select('/volume1/photos/gone.jpg')
    const backend = backendWith({ command: 'cleanup-plan', completion: 'failed', errors: [{ path: '/volume1/photos/gone.jpg', code: 'not_found', message: 'gone' }], warnings: [] })
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.text()).toContain('This item is no longer there')
    await wrapper.get('[data-test="drop-changed"]').trigger('click')
    expect(useBasket('/volume1').count.value).toBe(0)
    wrapper.unmount()
  })

  it('takes the duplicate group of a copy that is gone out of the list', async () => {
    const basket = useBasket('/volume1')
    basket.clear()
    const gone = 'a'.repeat(32)
    const other = 'b'.repeat(32)
    basket.addDuplicates([
      { id: gone, name: 'Music', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/main/Music', preferred: false, drive: false, shares: ['media'] },
      { id: other, name: 'Inbox', copies: 1, canFree: MIB, freesLater: 0, keeps: '/volume1/main/Inbox', preferred: false, drive: false, shares: ['media'] },
    ])
    const backend = backendWith({ command: 'cleanup-plan', completion: 'failed', errors: [{ path: '/volume1/media/Music', code: 'not_found', message: 'gone', group: gone }], warnings: [] })
    setBackend(backend as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.text()).toContain('This item is no longer there')
    await wrapper.get('[data-test="drop-changed"]').trigger('click')
    expect(basket.duplicates.value.groups.map(group => group.id)).toEqual([other])
    wrapper.unmount()
    basket.clear()
  })

  it('says how many symlinks the Recycle Bin does not keep, from the plan, and nothing when there are none', async () => {
    select('/volume1/photos/dtc', '/volume1/video/links')
    const photos = { ...share('photos', ['/volume1/photos/dtc'], { enabled: true, admin_only: true }), symlinks: 14 }
    // Deleted for good anyway: its symlinks are not the Recycle Bin's to drop.
    const video = { ...share('video', ['/volume1/video/links'], { enabled: false, admin_only: false }), symlinks: 5 }
    setBackend(backendWith(planOf([photos, video])) as Backend)
    const wrapper = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(wrapper.get('[data-test="review-symlinks"]').text()).toBe("14 shortcuts (symlinks) among them are not kept by DSM's Recycle Bin and do not come back on restore")
    wrapper.unmount()

    select('/volume1/photos/a.jpg')
    setBackend(backendWith(planOf([share('photos', ['/volume1/photos/a.jpg'], { enabled: true, admin_only: true })])) as Backend)
    const plain = mount(ReviewDialog, { props: { open: true, volume: '/volume1' }, global: { stubs } })
    await flushPromises()
    expect(plain.find('[data-test="review-recycle"]').exists()).toBe(true)
    expect(plain.find('[data-test="review-symlinks"]').exists()).toBe(false)
    plain.unmount()
  })
})
