import { withPlanningFixtureJobs } from '@/api/planningFixtureJobs'
import type { Backend } from '@/api/methods'
/* eslint vue/one-component-per-file: off */
import { defineComponent } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { vi } from 'vitest'
import { fixtureBackend } from '@/api/fixtures'
import { setBackend as setPlainBackend } from '@/api'
import { declineRootWork } from '@/authorizationFixture'
import RootfsCleanupDialog from './RootfsCleanupDialog.vue'

const setBackend = (backend: Backend) => setPlainBackend(withPlanningFixtureJobs(backend))

const DialogStub = defineComponent({
  props: {
    open: Boolean,
    title: { type: String, default: '' },
    closeLabel: { type: String, default: '' },
  },
  emits: ['update:open'],
  template: '<section v-if="open" role="dialog" :aria-label="title"><slot /><slot name="footer" /></section>',
})

const ButtonStub = defineComponent({
  props: {
    disabled: Boolean,
    loading: Boolean,
    variant: { type: String, default: '' },
  },
  emits: ['click'],
  template: '<button :disabled="disabled || loading" @click="$emit(\'click\')"><slot /></button>',
})

const CheckboxStub = defineComponent({
  props: {
    modelValue: Boolean,
    label: { type: String, default: '' },
    disabled: Boolean,
  },
  emits: ['update:modelValue'],
  template: '<label><input type="checkbox" :checked="modelValue" :disabled="disabled" @change="$emit(\'update:modelValue\', $event.target.checked)" />{{ label }}</label>',
})

const RootfsApplyStepStub = defineComponent({
  props: { open: Boolean, planId: { type: String, default: null } },
  template: '<div v-if="open" data-testid="apply-step">Apply step {{ planId }}</div>',
})

const mountDialog = () => mount(RootfsCleanupDialog, {
  global: { stubs: { Dialog: DialogStub, Button: ButtonStub, Checkbox: CheckboxStub, RootfsApplyStep: RootfsApplyStepStub } },
})

const clickTextButton = async (wrapper: ReturnType<typeof mountDialog>, label: string) => {
  const button = wrapper.findAll('button').find(candidate => candidate.text() === label)
  if (!button) throw new Error(`button not found: ${label}`)
  await button.trigger('click')
}

describe('RootfsCleanupDialog', () => {
  it('previews KB caches and upload images with per-category sizes and effects', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_cleanup' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    await wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="false"]').trigger('click')
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('GeoIP database cache'))
    expect(wrapper.text()).toContain('Desktop and login images')
    expect(wrapper.text()).toContain('Winbind cache')
    expect(wrapper.text()).toContain('Restart Winbind manually')
    expect(wrapper.text()).toContain('80 MB')
    expect(wrapper.text()).toContain('Users may need to upload')
    wrapper.unmount()
  })

  it('previews the administrator tool caches, the Hyper Backup installer and DSM’s old-log deletion in deep mode', async () => {
    setBackend(fixtureBackend({ scenario: 'sysvol_deep_short' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    await wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="false"]').trigger('click')
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Package caches of administrator tools'))
    const text = wrapper.text()
    expect(text).toContain('Download caches of tools the administrator installed under /root (such as npm, Bun and pip) move')
    expect(text).toContain('Hyper Backup restore installer')
    expect(text).toContain('Hyper Backup cannot restore the entire system unless you copy it back')
    expect(text).toContain('Delete logs older than DSM keeps')
    expect(text).toContain('9.1 MB')
    expect(text).toContain('logging may pause. Released space cannot be estimated in advance')
    expect(text).toContain('that history is deleted. Released space cannot be estimated in advance')
    const deleteLog = wrapper.findAll('.rootfs-clean__tools li').find(item => item.text().includes('Delete logs older'))!
    expect(deleteLog.find('.rootfs-clean__tool-size').text()).toBe('9.1 MB')
    expect(deleteLog.find('.rootfs-clean__tool-size').element.previousElementSibling?.textContent).toContain('there is no copy')
    expect(text).not.toContain('Other files')
    expect(text).not.toContain('DSM maintenance tool')
    wrapper.unmount()
  })

  it('asks for room for what deep mode moves when deep is chosen', async () => {
    const backend = fixtureBackend({ scenario: 'sysvol_deep_short' })
    setBackend(async (method, params) => {
      const document = (await backend(method, params)) as { volumes?: { free_bytes: number }[] }
      // 400 MB free: room for Standard's 305.2 MB, not for Deep's 503.6 MB.
      if (method === 'volumes.list') document.volumes?.forEach(volume => (volume.free_bytes = 400_000_000))
      return document
    })
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    expect(wrapper.text()).not.toContain('Not enough free space')
    await wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="false"]').trigger('click')
    expect(wrapper.text()).toContain('Not enough free space (needs 503.6 MB)')
    wrapper.unmount()
  })

  it('defaults to standard, sends an explicit deep choice and requires its extra confirmation', async () => {
    const calls: unknown[] = []
    const backend = fixtureBackend({ scenario: 'rootfs_cleanup' })
    setBackend((method, params) => {
      if (method === 'rootfs.plan') calls.push(params)
      return backend(method, params)
    })
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    expect(wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="true"]').element.closest('label')!.textContent).toContain('Standard')
    await wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="false"]').trigger('click')
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(calls).toHaveLength(1))
    expect(calls[0]).toMatchObject({ mode: 'deep' })
    await vi.waitFor(() => expect(wrapper.text()).toContain('What will be moved'))
    expect(wrapper.findAll('input[type="checkbox"]')).toHaveLength(2)
    await wrapper.findAll('input[type="checkbox"]')[0]!.setValue(true)
    expect(wrapper.findAll('button').find(b => b.text() === 'Yes, clean up')!.element.disabled).toBe(true)
    await wrapper.findAll('input[type="checkbox"]')[1]!.setValue(true)
    await clickTextButton(wrapper, 'Yes, clean up')
    expect(wrapper.find('[data-testid="apply-step"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('clears deep consent after going back and defaults to standard when reopened', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_cleanup' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    await wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="false"]').trigger('click')
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.findAll('input[type="checkbox"]')).toHaveLength(2))
    for (const input of wrapper.findAll('input[type="checkbox"]')) await input.setValue(true)
    await clickTextButton(wrapper, 'Back')
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.findAll('input[type="checkbox"]')).toHaveLength(2))
    expect(wrapper.findAll('input[type="checkbox"]').every(input => !(input.element as HTMLInputElement).checked)).toBe(true)
    await wrapper.vm.open()
    await flushPromises()
    expect(wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="true"]').element.closest('label')!.textContent).toContain('Standard')
    wrapper.unmount()
  })

  it('refuses a preview for a different mode before showing confirmation', async () => {
    const backend = fixtureBackend({ scenario: 'rootfs_cleanup' })
    setBackend(async (method, params) => {
      const document = await backend(method, params)
      return method === 'job.result' && 'command' in document && document.command === 'rootfs-plan' ? { ...document, mode: 'deep' } : document
    })
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Contact Synology support'))
    expect(wrapper.find('input[type="checkbox"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('opens U4 after the user confirms the no-undo step', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_cleanup' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('What will be moved'))

    await wrapper.find('input[type="checkbox"]').setValue(true)
    await clickTextButton(wrapper, 'Yes, clean up')

    expect(wrapper.find('[data-testid="apply-step"]').text()).toContain('Apply step rootfs-')
  })

  it('neither plans nor applies when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'rootfs_cleanup' })
    setBackend((method, params) => {
      calls.push(method)
      return recorded(method, params)
    })
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()
    let undo = declineRootWork()
    try {
      await clickTextButton(wrapper, 'Next')
      await flushPromises()
      expect(calls).not.toContain('rootfs.plan')
      expect(wrapper.text()).toContain('Where should the files go')

      undo()
      await clickTextButton(wrapper, 'Next')
      await vi.waitFor(() => expect(wrapper.text()).toContain('What will be moved'))
      undo = declineRootWork()
      await wrapper.find('input[type="checkbox"]').setValue(true)
      await clickTextButton(wrapper, 'Yes, clean up')
      await flushPromises()
      expect(wrapper.find('[data-testid="apply-step"]').exists()).toBe(false)
      expect(wrapper.text()).toContain('What will be moved')
    } finally {
      undo()
    }
  })

  it('opens step 1, reads volumes and preselects the roomiest eligible share', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_cleanup' }))
    const wrapper = mountDialog()

    await wrapper.vm.open()
    await flushPromises()

    expect(wrapper.text()).toContain('Where should the files go')
    expect(wrapper.text()).toContain('backup')
    expect(wrapper.find('[data-testid="rootfs-destination"] [role="radio"][aria-checked="true"]').element.closest('label')!.textContent).toContain('backup')
    expect(wrapper.text()).toContain('Not enough free space (needs 366 MB)')
    expect(wrapper.text()).not.toContain('A scan or cleanup is running on volume3')
    const scanningDestination = wrapper.find('[data-testid="rootfs-destination"] [role="radio"][value="/volume3/archive"]')
    expect(scanningDestination.attributes('disabled')).toBeUndefined()
    await scanningDestination.trigger('click')
    expect(scanningDestination.attributes('aria-checked')).toBe('true')
    expect(wrapper.text()).toContain('Files moved here may only appear in the next scan result')
  })

  it('shows the plan contents, consequences, kept reasons, and a small-file aggregate on step 2', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_cleanup' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('What will be moved'))

    expect(wrapper.find('.rootfs-clean__category-list th[aria-sort]').exists()).toBe(true)
    expect(wrapper.find('.rootfs-clean__category-list').text()).toMatch(/\d+ items/)
    expect(wrapper.text()).toContain('What will be moved')
    expect(wrapper.text()).toContain('Leftovers from an unfinished DSM update')
    expect(wrapper.findAll('.rootfs-clean__stats [data-ui-statistic-value]')).toHaveLength(3)
    // Each figure is a DSM inset block, the UI's own grey area inside a card.
    expect(wrapper.findAll('.rootfs-clean__stats > [data-ui-surface][data-variant="inset"] [data-ui-statistic-value]')).toHaveLength(3)
    expect(wrapper.text()).toContain('DSM will also')
    expect(wrapper.text()).toContain('There is no undo')
    expect(wrapper.text()).toContain('Modified in the last 7 days; kept for support')
    expect(wrapper.text()).toContain('files under 1 MiB')
    expect(wrapper.text()).toContain('I understand there is no undo')
    expect(wrapper.findAll('button').some(button => button.text() === 'Yes, clean up' && button.element.disabled)).toBe(true)
  })

  it('keeps the selected share and explains a destination_full refusal on step 1', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_destination_full' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Not enough free space in that shared folder'))

    expect(wrapper.find('[data-testid="rootfs-destination"] [role="radio"][aria-checked="true"]').element.closest('label')!.textContent).toContain('backup')
  })

  it.each([
    ['rootfs_walk_stopped', 'Couldn’t check everything; try again later'],
    ['rootfs_dsm_update_active', 'A DSM update is in progress. Finish or cancel it in Control Panel > Update & Restore, then check again'],
    ['rootfs_stale', 'Something changed. Check again'],
  ])('shows the %s plan refusal', async (scenario, message) => {
    setBackend(fixtureBackend({ scenario }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain(message))
  })

  it('describes the rootfs writer lock without claiming a volume scan is running', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_busy' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Another system partition cleanup is already running.'))
    expect(wrapper.text()).not.toContain('This volume is in use by a scan or cleanup.')
  })

  it('shows the no-shares callout and keeps Next disabled', async () => {
    setBackend(fixtureBackend({ scenario: 'rootfs_no_shares' }))
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    expect(wrapper.text()).toContain('No shared folder found')
    expect(wrapper.findAll('button').some(button => button.text() === 'Next' && button.element.disabled)).toBe(true)
  })

  it('offers the shared folders of an ext4 volume as destinations, as rootfs plan accepts them', async () => {
    const backend = fixtureBackend({ scenario: 'rootfs_cleanup' })
    const ext4Only = async () => {
      const document = structuredClone(await backend('volumes.list', {}))
      document.volumes = document.volumes.slice(0, 1).map(volume => ({
        ...volume,
        filesystem: 'ext4' as const,
        capabilities: { ...volume.capabilities, snapshots: false, reflinks: false },
        pending_deletion: null,
        shares: volume.shares.slice(0, 1).map(share => ({ ...share, subvolume_id: null, inode: 12 })),
      }))
      return document
    }
    setBackend(async (method, params) => (method === 'volumes.list' ? ext4Only() : backend(method, params)) as never)
    const wrapper = mountDialog()
    await wrapper.vm.open()
    await flushPromises()

    expect(wrapper.text()).not.toContain('No shared folder found')
    const destination = wrapper.find('[data-testid="rootfs-destination"] [role="radio"][value="/volume1/backup"]')
    expect(destination.exists()).toBe(true)
    expect(destination.attributes('aria-checked')).toBe('true')
    expect(wrapper.findAll('button').some(button => button.text() === 'Next' && !button.element.disabled)).toBe(true)
  })
})


it('keeps destination and mode editable while a preview is pending and discards that stale preview', async () => {
  const backend = fixtureBackend({ scenario: 'rootfs_cleanup' })
  let finish!: () => void
  let planStarted = false
  setBackend(async (method, params) => {
    if (method === 'rootfs.plan') {
      planStarted = true
      await new Promise<void>(resolve => { finish = resolve })
    }
    return backend(method, params)
  })
  const wrapper = mountDialog()
  try {
    await wrapper.vm.open()
    await clickTextButton(wrapper, 'Next')
    await vi.waitFor(() => expect(planStarted).toBe(true))
    const options = wrapper.findAll('[data-testid="rootfs-mode"] [role="radio"]')
    expect(options.every(option => option.attributes('disabled') === undefined)).toBe(true)
    await options[1]!.trigger('click')
    finish()
    await flushPromises()
    expect(wrapper.text()).toContain('Where should the files go')
    expect(wrapper.text()).not.toContain('What will be moved')
  } finally { wrapper.unmount() }
})

it('shows selectable shares while the initial system report is still loading', async () => {
  const backend = fixtureBackend({ scenario: 'rootfs_cleanup' })
  let finish!: () => void
  setBackend(async (method, params) => {
    if (method === 'rootfs.report') await new Promise<void>(resolve => { finish = resolve })
    return backend(method, params)
  })
  const wrapper = mountDialog()
  try {
    const opening = wrapper.vm.open()
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.findAll('[data-testid="rootfs-destination"] [role="radio"]').length).toBeGreaterThan(0))
    const destinations = wrapper.findAll('[data-testid="rootfs-destination"] [role="radio"]')
    expect(destinations[0]!.attributes('disabled')).toBeUndefined()
    await destinations[0]!.trigger('click')
    await wrapper.findAll('[data-testid="rootfs-mode"] [role="radio"]')[1]!.trigger('click')
    expect(wrapper.text()).toContain('Checking…')
    expect(wrapper.findAll('button').find(button => button.text() === 'Next')!.attributes('disabled')).toBeDefined()
    finish()
    await opening
    expect(wrapper.find('[data-testid="rootfs-mode"] [role="radio"][aria-checked="true"]').element.closest('label')!.textContent).toContain('Deep')
  } finally { wrapper.unmount() }
})
