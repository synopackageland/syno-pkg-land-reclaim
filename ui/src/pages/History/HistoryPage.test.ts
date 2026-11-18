import { h } from 'vue'
import { AppRoot, ToastProvider } from '@synopackageland/ui'
import { flushPromises, mount } from '@vue/test-utils'
import { setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import { fixtureBackend } from '@/api/fixtures'
import { declineRootWork } from '@/authorizationFixture'
import { refreshJobs } from '@/stores/jobs'
import { loadVolumes, volumes } from '@/stores/volumes'
import days from '../../../fixtures/history.list/recorded-days.json'
import { PAGE_ACTIONS, showPageActionsInPlace } from '@/shell/pageActionsInPlace'
import HistoryPage from './HistoryPage.vue'
import VolumePicker from '@/components/VolumePicker.vue'

showPageActionsInPlace()

async function mountPage(scenario = 'interaction', backend: Backend = fixtureBackend({ scenario })) {
  setBackend(backend)
  volumes.selected = '/volume1'
  await loadVolumes()
  await refreshJobs()
  const wrapper = mount({ render: () => h(AppRoot, null, () => h(ToastProvider, null, () => h(HistoryPage))) }, { attachTo: document.body })
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
  return wrapper
}

type Page = Awaited<ReturnType<typeof mountPage>>

const pageButton = (wrapper: Page, text: string) => wrapper.findAll('button').find(candidate => candidate.text() === text)!

/** Overlays teleport out of the wrapper, into the dialog element. */
function dialogButton(text: string) {
  const dialog = document.querySelector('[role="dialog"]')
  const found = [...(dialog?.querySelectorAll('button') ?? [])].find(candidate => (candidate.textContent ?? '').trim() === text)
  if (!found) throw new Error(`no dialog button labelled ${text}`)
  return found
}

/** Three scans on one day whose used space differs by a few MB, as on a NAS rescanned in a row. */
function sameDayBackend(): Backend {
  const backend = fixtureBackend()
  const minutes = ['01:13:24', '01:16:19', '01:19:40']
  const scans = days.scans.slice(0, 3).map((scan, index) => ({
    ...scan,
    started: `2026-09-30T${minutes[index]}Z`,
    finished: `2026-09-30T${minutes[index]}Z`,
    df_used_bytes: 43_300_000_000 + index * 5_000_000,
  }))
  return async (method, params) => (method === 'history.list' ? ({ ...days, scans } as never) : backend(method, params))
}

const dialogText = () => document.querySelector('[role="dialog"]')?.textContent ?? ''

describe('HistoryPage', () => {
  it('opens the selected saved job result even when a newer volume report exists', async () => {
    const base = fixtureBackend()
    const calls: string[] = []
    const older = { ...days.scans[0]!, job_id: 'selected-old', targets: [{ path: '/volume1/selected-share' }], volume: false, outcome: 'completed', report: '/private/old.srrep' }
    const newer = { ...older, job_id: 'newest-volume', targets: [{ path: '/volume1' }], volume: true }
    const wrapper = await mountPage('interaction', (async (method, params) => {
      if (method === 'history.list') return { ...days, scans: [newer, older] }
      if (method === 'jobs.list') return { jobs: [] }
      if (method === 'job.result') {
        calls.push((params as { job_id: string }).job_id)
        return { ...await base('job.result', { job_id: 'fixture-job0001' }), job_id: 'selected-old' }
      }
      return base(method, params)
    }) as Backend)
    try {
      const row = wrapper.findAll('tbody tr').find(row => row.text().includes('selected-share'))!
      await row.findAll('button').find(button => button.text() === 'View result')!.trigger('click')
      await flushPromises()
      await vi.dynamicImportSettled()
      await flushPromises()
      expect(calls).toEqual(['selected-old'])
      expect(document.querySelector('[data-report-job-id="selected-old"]')).not.toBeNull()
      expect(document.querySelector('[data-report-job-id="newest-volume"]')).toBeNull()
    } finally { wrapper.unmount() }
  })
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('dates the newest finished scan in the header chip by when it finished', async () => {
    const backend = fixtureBackend()
    const now = Date.now()
    const scan = { ...days.scans[0], started: new Date(now - 60 * 60_000).toISOString(), finished: new Date(now - 2 * 60_000).toISOString(), outcome: 'completed' }
    const wrapper = await mountPage('interaction', async (method, params) =>
      method === 'history.list' ? ({ ...days, scans: [scan] } as never) : method === 'jobs.list' ? ({ ...(await backend(method, params)), jobs: [] } as never) : backend(method, params))
    const chip = document.querySelector(PAGE_ACTIONS)?.textContent ?? ''
    expect(chip).toContain('2 minutes ago')
    expect(chip).not.toContain('hour')
    wrapper.unmount()
  })

  it('uses a neutral badge for small refs while preserving the stored partial outcome', async () => {
    const backend = fixtureBackend({ scenario: 'unknown_small' })
    const saved = await backend('history.list', {})
    expect(saved.scans.some(scan => scan.outcome === 'partial')).toBe(true)
    const wrapper = await mountPage('unknown_small', backend)
    expect(wrapper.text()).toContain('Small amount unattributed')
    expect(wrapper.text()).not.toContain('The newest scan is partial')
    expect(saved.scans.some(scan => scan.outcome === 'partial')).toBe(true)
    wrapper.unmount()
  })

  it('shows a neutral changed-during-scan badge for minor namespace churn, and keeps the partial badge for eleven entries', async () => {
    const backend = fixtureBackend({ scenario: 'namespace_churn' })
    const wrapper = await mountPage('namespace_churn', backend)
    expect(wrapper.text()).toContain('Changed during scan')
    expect(wrapper.text()).not.toContain('The newest scan is partial')
    wrapper.unmount()
    document.body.innerHTML = ''
    const many = await mountPage('namespace_churn_many', fixtureBackend({ scenario: 'namespace_churn_many' }))
    expect(many.text()).toContain('The newest scan is partial')
    many.unmount()
  })

  it.each(['cleanup-apply', 'rootfs-apply'])('labels interrupted %s and never offers scan resume', async kind => {
    const base = fixtureBackend({ scenario: 'interaction' })
    const wrapper = await mountPage('interaction', (async (method, params) => {
      if (method === 'jobs.list') return { jobs: [{
        kind, job_id: 'apply-lost', state: 'interrupted', phase: 'execute',
        targets: [{ path: kind === 'rootfs-apply' ? '/var/lib/synoreclaim/rootfs-plans/rootfs-1-2.json' : '/volume1/share/large.bin' }], filesystems: [{ fsid: 'fs', mount: { path: '/volume1' } }],
        created: '2026-10-01T00:00:00Z', updated: '2026-10-01T00:00:00Z',
        result: null, reason: null, profile: 'low',
      }] } as never
      return base(method, params)
    }) as Backend)
    try {
      const row = wrapper.findAll('.history-grid tbody tr').find(row => row.text().includes('Operation interrupted'))!
      expect(row.text()).toContain(kind === 'cleanup-apply' ? 'Move to Recycle Bin' : 'System partition cleanup')
      expect(row.find('[data-ui-cell-status] + [data-ui-cell-secondary]').text()).toBe('Operation interrupted; some work may have completed')
      expect(row.text()).not.toContain('Resume scan')
      expect(row.text()).not.toContain('rootfs-1-2.json')
      if (kind === 'cleanup-apply') expect(row.text()).toContain('Recheck')
    } finally { wrapper.unmount() }
  })

  it('lists the recorded scans with their scope, used space and outcome', async () => {
    const wrapper = await mountPage()
    expect(wrapper.findAll('.history-grid tbody tr').length).toBeGreaterThan(1)
    expect(wrapper.text()).toContain('Whole volume1')
    expect(wrapper.text()).toContain('Replaced')
    wrapper.unmount()
  })

  it('shows the captured whole-volume partial scan', async () => {
    const wrapper = await mountPage('normal')
    expect(wrapper.findAll('.history-grid tbody tr')).toHaveLength(1)
    expect(wrapper.text()).toContain('Whole volume1')
    expect(wrapper.text()).toContain('Partial')
    wrapper.unmount()
  })

  it('shows one used figure, not a trend, when every scan finished at once', async () => {
    const wrapper = await mountPage()
    expect(wrapper.text()).toContain('9.6 MB used')
    expect(wrapper.text()).toContain('The trend starts with the next scan')
    expect(wrapper.text()).not.toContain('No used-space figures')
    expect(wrapper.find('.line-chart').exists()).toBe(false)
    wrapper.unmount()
  })

  it('states the trend span in days', async () => {
    const wrapper = await mountPage('days')
    expect(wrapper.text()).toContain('9.6 MB → 9.6 MB over 3 days')
    wrapper.unmount()
  })

  it('draws the used-space trend when scans are days apart', async () => {
    const wrapper = await mountPage('days')
    expect(wrapper.find('.line-chart title').text()).toBe('Used space on volume1 at each scan')
    expect(wrapper.find('.line-chart-value').text()).toBe('9.6 MB')
    expect(wrapper.findAll('.line-chart-axis span')).toHaveLength(4)
    wrapper.unmount()
  })

  it('labels a trend of one day with distinct times of day', async () => {
    const wrapper = await mountPage('normal', sameDayBackend())
    const labels = wrapper.findAll('.line-chart-axis span').map(span => span.text())
    expect(labels).toHaveLength(3)
    expect(new Set(labels).size).toBe(3)
    for (const label of labels) expect(label).toMatch(/^\d{2}:\d{2}$/)
    wrapper.unmount()
  })

  it('gives the span of same-day scans in minutes, not as a day', async () => {
    const wrapper = await mountPage('normal', sameDayBackend())
    expect(wrapper.text()).toContain('40.3 GB → 40.3 GB over 6 minutes')
    wrapper.unmount()
  })

  it('draws a change of a few MB on 43 GB as a flat line, not a ramp from the bottom', async () => {
    const wrapper = await mountPage('normal', sameDayBackend())
    const cy = Number(wrapper.find('.line-chart circle').attributes('cy'))
    const height = Number(wrapper.find('.line-chart svg').attributes('height'))
    expect(cy).toBeGreaterThan(height * 0.4)
    expect(cy).toBeLessThan(height * 0.6)
    wrapper.unmount()
  })

  it('merges a paused job from jobs.list and offers to resume it', async () => {
    const wrapper = await mountPage('paused')
    expect(wrapper.text()).toContain('Paused')
    expect(pageButton(wrapper, 'Resume').exists()).toBe(true)
    wrapper.unmount()
  })

  it('warns when the newest scan of the volume is partial', async () => {
    const wrapper = await mountPage('partial')
    expect(wrapper.text()).toContain('The newest scan is partial')
    wrapper.unmount()
  })

  it('explains why a scan was replaced', async () => {
    const wrapper = await mountPage()
    await pageButton(wrapper, 'Why').trigger('click')
    await flushPromises()
    expect(dialogText()).toContain('About this scan: Replaced')
    expect(dialogText()).toContain('A newer scan of the same shared folders')
    wrapper.unmount()
  })

  it('explains why a partial scan is partial', async () => {
    const wrapper = await mountPage('partial')
    await pageButton(wrapper, 'Why').trigger('click')
    await flushPromises()
    expect(dialogText()).toContain('About this scan: Partial')
    expect(dialogText()).toContain('Some data could not be read')
    wrapper.unmount()
  })

  it('shows the empty state and the scan action when nothing was scanned', async () => {
    const wrapper = await mountPage('no_report')
    expect(wrapper.text()).toContain('No scans recorded yet')
    expect(pageButton(wrapper, 'Scan now').exists()).toBe(true)
    wrapper.unmount()
  })

  it('shows the error state when history.list fails', async () => {
    const wrapper = await mountPage('error')
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.text()).toContain('The Syno Reclaim command failed')
    wrapper.unmount()
  })

  it('shows the saved results of the volume picked for the trend', async () => {
    const fixtures = fixtureBackend()
    // The recorded volume1 again as an unscanned volume2.
    const backend: Backend = async (method, params) => {
      const answer = await fixtures(method, params)
      if (method !== 'volumes.list') return answer
      const list = answer as { volumes: { mount: { path: string }; reports: number }[] }
      list.volumes.push({ ...structuredClone(list.volumes[0]), mount: { path: '/volume2' }, reports: 0 })
      return answer
    }
    const wrapper = await mountPage('normal', backend)
    wrapper.findComponent(VolumePicker).vm.$emit('update:modelValue', '/volume2')
    await flushPromises()
    const results = wrapper.find('.results').text()
    expect(results).toContain('ResultsNone yet')
    expect(results).toContain('Scan cacheNone yet')
    expect(results).not.toContain('Reusable')
    expect(wrapper.find('.results-card').text()).toContain('volume2')
    wrapper.unmount()
  })

  it('explains a disabled cache clear once, replacing the hint', async () => {
    const wrapper = await mountPage('normal')
    const card = wrapper.find('.results-card')
    expect(card.find('.results').text()).toMatch(/Scan cache.*·.*items/)
    expect(card.find('.results').text()).toMatch(/Reusable\d+ of \d+/)
    const reasons = ['A scan is running; clear the cache after it finishes.', 'Cached data is reused by the next scan only if it is unchanged.']
    const shown = reasons.filter(reason => card.text().includes(reason))
    expect(shown).toHaveLength(1)
    wrapper.unmount()
  })

  it('clears the cache only after the confirmation', async () => {
    const wrapper = await mountPage()
    await pageButton(wrapper, 'Clear cache').trigger('click')
    await flushPromises()
    expect(dialogText()).toContain('Scan results are kept')
    dialogButton('Clear cache').click()
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).toContain('Cleared 5 cached items')
    wrapper.unmount()
  })

  it('keeps the cache and the confirmation when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'interaction' })
    const wrapper = await mountPage('interaction', (method, params) => {
      calls.push(method)
      return recorded(method, params)
    })
    await pageButton(wrapper, 'Clear cache').trigger('click')
    await flushPromises()
    const undo = declineRootWork()
    try {
      dialogButton('Clear cache').click()
      await flushPromises()
      expect(calls).not.toContain('cache.clear')
      expect(dialogText()).toContain('Scan results are kept')
      expect(wrapper.text()).not.toContain('Could not')
    } finally {
      undo()
      wrapper.unmount()
    }
  })
})
