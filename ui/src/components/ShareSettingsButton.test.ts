import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const launch = vi.hoisted(() => vi.fn())
vi.mock('@synopackageland/sdk', async importOriginal => ({
  ...await importOriginal<typeof import('@synopackageland/sdk')>(),
  openControlPanel: launch,
}))

import ShareSettingsButton from './ShareSettingsButton.vue'
import { openShareSettings } from './shareSettings'

describe('ShareSettingsButton', () => {
  beforeEach(() => launch.mockReset())

  it('opens Control Panel › Shared Folder with the share selected', async () => {
    launch.mockResolvedValue({ status: 'requested' })
    const wrapper = mount(ShareSettingsButton, { props: { share: 'Archive' } })
    expect(wrapper.text()).toBe('Shared folder settings')
    expect(wrapper.text()).not.toMatch(/\.\.\.|…/)
    await wrapper.get('[data-testid="share-settings-Archive"]').trigger('click')
    await flushPromises()
    expect(launch).toHaveBeenCalledWith({ page: 'shared-folder', select: 'Archive' })
  })

  it('names its share when several buttons stand side by side', () => {
    const wrapper = mount(ShareSettingsButton, { props: { share: 'Archive', named: true } })
    expect(wrapper.text()).toBe('Archive settings')
  })

  it('says when DSM launched nothing', async () => {
    launch.mockResolvedValue({ status: 'unavailable' })
    expect(await openShareSettings('Archive')).toBe(false)
    launch.mockRejectedValue(new Error('no DSM'))
    expect(await openShareSettings('Archive')).toBe(false)
    launch.mockResolvedValue({ status: 'requested' })
    expect(await openShareSettings('Archive')).toBe(true)
  })
})
