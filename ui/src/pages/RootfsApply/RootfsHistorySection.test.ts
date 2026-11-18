import { flushPromises, mount } from '@vue/test-utils'
import { volumes } from '@/stores/volumes'
import shareVolumes from '../../../fixtures/volumes.list/rootfs-cleanup.json'
import { defineComponent } from 'vue'
import { AppRoot, ToastProvider } from '@synopackageland/ui'
import recorded from '../../../fixtures/rootfs.history/recorded.json'
import RootfsHistorySection from './RootfsHistorySection.vue'
import type { RootfsHistoryRecord } from './rootfsTypes'

const applies = (recorded as unknown as { applies: RootfsHistoryRecord[] }).applies

const Host = defineComponent({
  components: { AppRoot, ToastProvider, RootfsHistorySection },
  props: {
    applies: { type: Array as () => RootfsHistoryRecord[], required: true },
    canCleanUp: { type: Boolean, default: false },
  },
  template: `
    <AppRoot>
      <ToastProvider>
        <RootfsHistorySection :applies="applies" :can-clean-up="canCleanUp" />
      </ToastProvider>
    </AppRoot>
  `,
})

describe('RootfsHistorySection', () => {
  // File Station opens only DSM's listed shared folders; the app loads them at start.
  beforeEach(() => {
    volumes.document = shareVolumes as never
  })
  afterEach(() => {
    volumes.document = null
  })

  it('hides the section when there are no cleanup records', () => {
    const wrapper = mount(Host, { props: { applies: [] } })
    expect(wrapper.find('.rootfs-history').exists()).toBe(false)
    wrapper.unmount()
  })

  it('exposes the complete moved-folder path as a tooltip', () => {
    const wrapper = mount(Host, { props: { applies } })
    expect(wrapper.find('.folder-link').attributes('title')).toBe(applies[0].move_dir.path)
    wrapper.unmount()
  })

  it('opens a moved folder in File Station as a shared-folder path, from the list and the detail', async () => {
    document.body.innerHTML = ''
    const calls: { id: string; params?: unknown }[] = []
    ;(window as unknown as { SYNO?: unknown }).SYNO = { SDS: { AppLaunch: (id: string, params?: unknown) => calls.push({ id, params }) } }
    const wrapper = mount(Host, { attachTo: document.body, props: { applies: [applies[0]] } })
    await wrapper.find('button.folder-link').trigger('click')
    await wrapper.findAll('button').find(button => button.text() === 'View')!.trigger('click')
    await flushPromises()
    const open = [...document.body.querySelectorAll<HTMLButtonElement>('.detail-card button')].find(button => button.textContent === 'Open in File Station')
    open!.click()
    const opendir = { opendir: applies[0].move_dir.path.replace(/^\/volume1/, '') }
    expect(calls).toEqual([
      { id: 'SYNO.SDS.App.FileStation3.Instance', params: opendir },
      { id: 'SYNO.SDS.App.FileStation3.Instance', params: opendir },
    ])
    delete (window as unknown as { SYNO?: unknown }).SYNO
    wrapper.unmount()
  })

  it('does not offer a moved folder outside a shared folder', async () => {
    document.body.innerHTML = ''
    const outside = { ...applies[0], move_dir: { path: '/volume1/@synoreclaim/moved' } }
    const wrapper = mount(Host, { attachTo: document.body, props: { applies: [outside] } })
    expect(wrapper.find('button.folder-link').exists()).toBe(false)
    expect(wrapper.find('.folder-link').text()).toContain('moved')
    await wrapper.findAll('button').find(button => button.text() === 'View')!.trigger('click')
    await flushPromises()
    const open = [...document.body.querySelectorAll<HTMLButtonElement>('.detail-card button')].find(button => button.textContent === 'Open in File Station')
    expect(open?.disabled).toBe(true)
    wrapper.unmount()
  })

  it('shows each cleanup result as a status cell', () => {
    const wrapper = mount(Host, { props: { applies } })
    expect(wrapper.findAll('tbody [data-ui-cell-status]')).toHaveLength(applies.length)
    wrapper.unmount()
  })

  it('opens a read-only detail and offers cleanup only while abnormal', async () => {
    document.body.innerHTML = ''
    const normal = mount(Host, { attachTo: document.body, props: { applies, canCleanUp: false } })
    await normal.findAll('button').find(button => button.text() === 'View')!.trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain(applies[0].move_dir.path)
    expect(document.querySelectorAll('.detail-stats [data-ui-statistic-value]')).toHaveLength(2)
    expect(document.body.textContent).not.toContain('Check and clean up')
    normal.unmount()

    document.body.innerHTML = ''
    const abnormal = mount(Host, { attachTo: document.body, props: { applies, canCleanUp: true } })
    await abnormal.findAll('button').find(button => button.text() === 'View')!.trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('Check and clean up')
    abnormal.unmount()
  })
})
