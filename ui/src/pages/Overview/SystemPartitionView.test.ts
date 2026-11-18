import { mount } from '@vue/test-utils'
import { Callout, LoadingArea } from '@synopackageland/ui'
import { ApiError } from '@/api'
import type { RootfsReportDocument } from '@/types'
import deepShort from '../../../fixtures/rootfs.report/deep-short.json'
import gettingFull from '../../../fixtures/rootfs.report/getting-full.json'
import nothingMovable from '../../../fixtures/rootfs.report/nothing-movable.json'
import partialReport from '../../../fixtures/rootfs.report/partial.json'
import smallReport from '../../../fixtures/rootfs.report/small.json'
import SystemPartitionView from './SystemPartitionView.vue'
import RootfsCleanupDialog from '@/rootfs/RootfsCleanupDialog.vue'

const doc = (value: unknown) => value as unknown as RootfsReportDocument

/** The view is presentational: Overview owns the ipc and passes the answer down. */
function mountView(report: RootfsReportDocument | null, options: { loading?: boolean; error?: ApiError | null } = {}) {
  return mount(SystemPartitionView, { props: { report, loading: options.loading ?? false, error: options.error ?? null } })
}

const button = (wrapper: ReturnType<typeof mountView>, text: string) => wrapper.findAll('button').find(b => b.text().startsWith(text))!

describe('SystemPartitionView', () => {
  it('shows the state card and the movable card', () => {
    const wrapper = mountView(doc(gettingFull))
    const text = wrapper.text()
    expect(text).toContain('System partition')
    expect(text).toContain('Getting full')
    expect(text).toContain('Clean up before the next DSM update')
    expect(text).toContain('7.1 GB used of 8 GB · 886.9 MB free')
    expect(text).toContain('89% used')
    expect(wrapper.get('[role="progressbar"]').attributes('data-tone')).toBe('warning')
    expect(text).toContain('Synology recommends keeping it under 70% used')
    expect(text).toContain('Can be moved off it')
    expect(text).toContain('110 MB')
    expect(text).toContain('Leftovers from an unfinished DSM update')
    expect(text).toContain('100 MB')
    expect(text).toContain('Old compressed logs')
    expect(text).toContain("+ DSM's own log cleanup")
    expect(text).toContain('Files are moved to a shared folder you pick')
    wrapper.unmount()
  })

  it('adds what a deep cleanup can move to the standard total', () => {
    const wrapper = mountView(doc(deepShort))
    const card = wrapper.get('.rootfs-movable')
    expect(card.text()).toContain('Standard cleanup')
    expect(card.text()).toContain('49.2 MB')
    expect(card.text()).toContain('Deep cleanup: up to 247.6 MB')
    expect(card.text()).toContain('Package caches of administrator tools')
    expect(card.text()).toContain('94.7 MB')
    expect(card.text()).toContain('Hyper Backup restore installer')
    expect(card.text()).toContain('Firewall location database')
    expect(wrapper.findAll('button').some(b => b.text().startsWith('Clean up…'))).toBe(true)
    wrapper.unmount()
  })

  it('offers deep cleanup when only deep mode can move something', () => {
    const onlyDeep = structuredClone(doc(deepShort))
    onlyDeep.cleanable = { ...onlyDeep.cleanable, bytes: 0, items: 0, by_category: [] }
    const wrapper = mountView(onlyDeep)
    const card = wrapper.get('.rootfs-movable')
    expect(card.text()).toContain('Deep cleanup: up to 247.6 MB')
    expect(card.text()).not.toContain('Standard cleanup')
    expect(wrapper.findAll('button').some(b => b.text().startsWith('Clean up…'))).toBe(true)
    wrapper.unmount()
  })

  it('says what is left to do when even a deep cleanup leaves too little room', () => {
    const wrapper = mountView(doc(deepShort))
    const callout = wrapper.findAllComponents(Callout).find(c => c.classes().includes('rootfs-short'))!
    const text = callout.text()
    expect(text).toContain('About 453.8 MB would be free after cleanup. A DSM update needs about 495 MB, and up to 1 GB in some cases')
    expect(text).toContain('278 MB is software you installed in /usr/local. Install it in a shared folder instead')
    expect(text).toContain('11.3 MB in /root is your own files, beyond the caches cleanup moves.')
    expect(text).toContain('Keep data in shared folders only')
    expect(text).toContain('Log system status periodically')
    expect(text).not.toContain('Restart')
    const link = callout.find('a[target="_blank"]')
    expect(link.attributes('href')).toContain('DSM_update_insufficient_system_capacity_for_update')
    expect(text).toContain("This NAS's system partition is only 2.3 GB. Synology's article explains the other options, including a reset")
    wrapper.unmount()
  })

  it('has no shortfall callout when the cleanup leaves room for an update', () => {
    const wrapper = mountView(doc(gettingFull))
    expect(wrapper.findAllComponents(Callout).some(c => c.classes().includes('rootfs-short'))).toBe(false)
    wrapper.unmount()
  })

  it('opens the mounted cleanup dialog from the system partition view', async () => {
    const wrapper = mountView(doc(gettingFull))
    const cleanUp = button(wrapper, 'Clean up…')
    expect(cleanUp.attributes('disabled')).toBeUndefined()
    expect(cleanUp.attributes('title')).toBeUndefined()
    expect(wrapper.findComponent(RootfsCleanupDialog).exists()).toBe(true)
    wrapper.unmount()
  })

  it('groups the owners into packages, cleanable items, DSM system and Other', () => {
    const wrapper = mountView(doc(gettingFull))
    const table = wrapper.find('table')
    expect(wrapper.text()).toContain('What uses the space')
    const items = wrapper.findAll('tbody tr').map(row => row.find('td').text())
    expect(items).toEqual(['Other', 'Package: SynoFinder', 'DSM system', 'Leftovers from an unfinished DSM update', 'Old compressed logs'].map(item => expect.stringContaining(item)))
    expect(table.text()).toContain('Package Center')
    expect(table.text()).toContain('Clean up')
    wrapper.unmount()
  })

  it('offers no advice-only action: only packages and cleanable rows have one', () => {
    const wrapper = mountView(doc(gettingFull))
    const actions = wrapper.findAll('tbody tr').map(row => row.findAll('td')[3].text())
    expect(actions).toEqual(['', 'Package Center', '', 'Clean up', 'Clean up'])
    expect(wrapper.text()).not.toContain('Move it yourself')
    expect(wrapper.text()).not.toContain('technical support')
    wrapper.unmount()
  })

  it('unfolds the read-only DSM system and Other rows into their parts', async () => {
    const wrapper = mountView(doc(gettingFull))
    const fold = (text: string) => wrapper.findAll('button.rootfs-fold').find(b => b.text().startsWith(text))!
    expect(fold('DSM system').attributes('aria-expanded')).toBe('false')

    await fold('DSM system').trigger('click')
    expect(fold('DSM system').attributes('aria-expanded')).toBe('true')
    expect(wrapper.text()).toContain('DSM manages these files. Leave them in place')
    expect(wrapper.text()).toContain('Firewall location database')

    await fold('Other').trigger('click')
    expect(wrapper.text()).toContain("Syno Reclaim doesn't move these files")
    expect(wrapper.text()).toContain('Installed by you (/usr/local)')
    expect(wrapper.text()).toContain('Unrecognized files')
    expect(wrapper.text()).toContain('2 smaller items (under 1 MB)')
    wrapper.unmount()
  })

  it('names a package by the name Package Center shows', () => {
    const named = structuredClone(doc(gettingFull))
    named.owners = named.owners.map(owner => (owner.name === 'SynoFinder' ? { ...owner, display_name: 'Universal Search' } : owner))
    const wrapper = mountView(named)
    expect(wrapper.find('table').text()).toContain('Package: Universal Search')
    wrapper.unmount()
  })

  it('shows the logs, files and deleted-but-open details', async () => {
    const wrapper = mountView(doc(gettingFull))
    await button(wrapper, 'Details').trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Deleted but still in use: 12 MB. Freed when syslog-ng restarts'))
    expect(wrapper.text()).toContain("Logs: 63.6 MB of DSM's 220 MB limit")
    expect(wrapper.text()).toContain('Files: 133,742 of 524,288')
    wrapper.unmount()
  })

  it('has no deleted-but-open line when nothing is held open', async () => {
    // Clone: the fixture is shared by every test in this file.
    const clean = structuredClone(doc(gettingFull))
    clean.open_deleted = { bytes_estimate: 0, holders: [], unreadable_processes: 0 }
    const wrapper = mountView(clean)
    await button(wrapper, 'Details').trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Files: 133,742 of 524,288'))
    expect(wrapper.text()).not.toContain('Deleted but still in use')
    wrapper.unmount()
  })

  it('warns that a stopped walk may undercount', () => {
    const wrapper = mountView(doc(partialReport))
    const callout = wrapper.findAllComponents(Callout).find(c => c.classes().includes('rootfs-partial'))!
    expect(callout.props('tone')).toBe('warning')
    expect(callout.text()).toContain('Some folders could not be read; sizes may be low')
    wrapper.unmount()
  })

  it('notes a small partition and links Synology’s article', () => {
    const wrapper = mountView(doc(smallReport))
    expect(wrapper.text()).toContain('This NAS has a small system partition.')
    const link = wrapper.find('a[target="_blank"]')
    expect(link.text()).toBe('Learn more')
    expect(link.attributes('href')).toContain('DSM_update_insufficient_system_capacity_for_update')
    wrapper.unmount()
  })

  it('names the largest owner when nothing can be moved', () => {
    const wrapper = mountView(doc(nothingMovable))
    expect(wrapper.text()).toContain("Syno Reclaim can't move anything here. Most of the space is used by: Installed by you (/usr/local)")
    expect(wrapper.text()).not.toContain('Can be moved off it')
    expect(wrapper.text()).not.toContain('Files are moved to a shared folder you pick')
    expect(wrapper.findAll('button').some(b => b.text().startsWith('Clean up…'))).toBe(false)
    wrapper.unmount()
  })

  it('rescans with the Rescan button', async () => {
    // The fixture was generated 2026-09-21; a month later it reads "last month" on any day the test runs.
    vi.useFakeTimers({ now: new Date('2026-10-21T14:13:20Z'), toFake: ['Date'] })
    try {
      const wrapper = mountView(doc(gettingFull))
      expect(wrapper.text()).toContain('Scanned last month')
      await button(wrapper, 'Rescan').trigger('click')
      expect(wrapper.emitted('refresh')).toHaveLength(1)
      wrapper.unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  it('keeps the shown report with a small indicator while it scans again', () => {
    const wrapper = mount(SystemPartitionView, { props: { report: doc(gettingFull), loading: false, refreshing: true, error: null } })
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(false)
    expect(wrapper.find('table').exists()).toBe(true)
    expect(wrapper.find('[role="status"]').text()).toBe('Updating…')
    expect(button(wrapper, 'Rescan').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('says so, and keeps the shown report, when scanning again fails', () => {
    const wrapper = mountView(doc(gettingFull), { error: new ApiError('busy') })
    expect(wrapper.find('table').exists()).toBe(true)
    expect(wrapper.find('[role="status"]').text()).toBe("Couldn't scan again")
    wrapper.unmount()
  })

  it('shows the loading state while it checks', () => {
    const wrapper = mountView(null, { loading: true })
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(true)
    expect(wrapper.text()).toContain('Checking the system partition…')
    wrapper.unmount()
  })

  it('shows a busy or failed answer with a retry that checks again', async () => {
    const busy = mountView(null, { error: new ApiError('busy') })
    expect(busy.text()).toContain('Another scan is using this volume')
    await button(busy, 'Retry').trigger('click')
    expect(busy.emitted('refresh')).toHaveLength(1)
    busy.unmount()

    const failed = mountView(null, { error: new ApiError('cli_error') })
    expect(failed.text()).toContain('The Syno Reclaim command failed')
    failed.unmount()
  })
})
