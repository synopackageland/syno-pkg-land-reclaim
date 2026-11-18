import { flushPromises, mount } from '@vue/test-utils'
import { ApiError, setBackend } from '@/api'
import WatchFreeSheet from './WatchFreeSheet.vue'

describe('WatchFreeSheet', () => {
  it('names the error when sampling fails', async () => {
    setBackend(async () => {
      throw new ApiError('timeout')
    })
    const wrapper = mount(WatchFreeSheet, { props: { open: true, volume: '/volume1' }, attachTo: document.body })
    await flushPromises()
    expect(document.body.textContent).toContain('The request took too long')
    wrapper.unmount()
  })
})
