import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it } from 'vitest'
import UsageBaselineHint from './UsageBaselineHint.vue'
import UsageChangeBanner from './UsageChangeBanner.vue'
import { usageChange } from './useUsageChange'
import { jobs } from '@/stores/jobs'
import { nav } from '@/stores/nav'
import { volumes } from '@/stores/volumes'
import doc from '../../fixtures/volumes.list/volume1.json'
import type { VolumesDocument } from '@/types'

// Storage Manager's GB: 1024³ bytes.
const GB = 1024 ** 3

beforeEach(() => {
  jobs.list = []
  nav.newScanOpen = false
  volumes.document = doc as VolumesDocument
  volumes.selected = '/volume1'
  usageChange.unavailable = []
  usageChange.alerts = [{ key: 'v1', mount: '/volume1', used: 500e9, delta: 50 * GB }]
})
afterEach(() => { usageChange.alerts = []; usageChange.unavailable = [] })

it('gives a change in the unit Storage Manager would show it in', () => {
  usageChange.alerts = [{ key: 'v1', mount: '/volume1', used: 500e9, delta: 512 * 1024 ** 2 }]
  expect(mount(UsageChangeBanner).text()).toContain('volume1: Usage has increased by 512 MB since the last scan')
})

it('shows one summary for increases and decreases and opens the existing scan dialog', async () => {
  usageChange.alerts.push({ key: 'v2', mount: '/volume2', used: 300e9, delta: -60 * GB })
  const wrapper = mount(UsageChangeBanner)
  expect(wrapper.findAll('.usage-banner')).toHaveLength(1)
  expect(wrapper.text()).toContain('volume1: Usage has increased by 50 GB since the last scan')
  expect(wrapper.text()).toContain('volume2: Usage has decreased by 60 GB since the last scan')
  await wrapper.findAll('button').find(button => button.text() === 'Rescan')!.trigger('click')
  expect(nav.newScanOpen).toBe(true)
  wrapper.unmount()
})

it('shows only a quiet hint when the selected volume has no comparable baseline', () => {
  usageChange.alerts = []
  usageChange.unavailable = ['/volume1']
  const banner = mount(UsageChangeBanner)
  expect(banner.find('.usage-banner').exists()).toBe(false)
  banner.unmount()
  const hint = mount(UsageBaselineHint)
  expect(hint.find('[data-ui-list-item-title]').text()).toBe('volume1')
  expect(hint.text()).toContain('Syno Reclaim can tell you about usage changes after the next complete scan')
  expect(hint.find('button').exists()).toBe(false)
  hint.unmount()
})

it('waits while a job is active', () => {
  jobs.list = [{ state: 'running' } as never]
  const wrapper = mount(UsageChangeBanner)
  expect(wrapper.find('.usage-banner').exists()).toBe(false)
  wrapper.unmount()
})
