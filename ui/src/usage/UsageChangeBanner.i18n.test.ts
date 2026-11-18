import { afterEach, describe, expect, it, vi } from 'vitest'
import type { VolumesDocument } from '@/types'
import doc from '../../fixtures/volumes.list/volume1.json'

// Storage Manager's GB: 1024³ bytes.
const GB = 1024 ** 3

// The app translator reads the DSM language once, when it is imported,
// so each locale needs a fresh module graph.
async function mountBanner(lang: 'cht' | 'chs') {
  vi.resetModules()
  window._S = (key: string) => (key === 'lang' ? lang : undefined)
  const { mount } = await import('@vue/test-utils')
  const { default: UsageChangeBanner } = await import('./UsageChangeBanner.vue')
  const { usageChange } = await import('./useUsageChange')
  const { jobs } = await import('@/stores/jobs')
  const { volumes } = await import('@/stores/volumes')
  jobs.list = []
  volumes.document = doc as VolumesDocument
  usageChange.alerts = [
    { key: 'v1', mount: '/volume1', used: 500e9, delta: 50 * GB },
    { key: 'v2', mount: '/volume2', used: 300e9, delta: -60 * GB },
  ]
  return mount(UsageChangeBanner)
}

afterEach(() => {
  vi.resetModules()
  window._S = undefined
})

describe('UsageChangeBanner summary in East Asian locales', () => {
  it('merges a zh-TW increase and decrease without a dot after 。', async () => {
    const wrapper = await mountBanner('cht')
    const text = wrapper.find('.usage-banner').text()
    expect(text).toContain('volume1：使用量較上次掃描增加 50 GB，結果可能已變動 · volume2：使用量較上次掃描減少 60 GB，結果可能已變動')
    expect(text).not.toContain('。 ·')
    wrapper.unmount()
  })

  it('merges a zh-CN increase and decrease without a dot after 。', async () => {
    const wrapper = await mountBanner('chs')
    const text = wrapper.find('.usage-banner').text()
    expect(text).toContain('volume1：使用量较上次扫描增加 50 GB，结果可能已变化 · volume2：使用量较上次扫描减少 60 GB，结果可能已变化')
    expect(text).not.toContain('。 ·')
    wrapper.unmount()
  })
})
