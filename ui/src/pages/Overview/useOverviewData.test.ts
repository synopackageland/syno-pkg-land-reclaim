import { afterEach } from 'vitest'
import { ApiError, setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import { answersVersion, invalidateAnswers } from '@/stores/answers'
import { useOverviewData } from './useOverviewData'

afterEach(() => setBackend(fixtureBackend()))

/** Let every already-settled microtask run, so a deferred reply can land. */
const flush = () => new Promise<void>(resolve => setTimeout(resolve, 0))

describe('useOverviewData', () => {
  it('retries an earlier no-report answer after a scan publishes a report', async () => {
    const noReport = fixtureBackend({ scenario: 'no_report' })
    const partial = fixtureBackend({ scenario: 'partial' })
    let overviewReads = 0
    setBackend(async (method, params) => {
      if (method === 'overview.get') {
        overviewReads += 1
        return overviewReads === 1 ? noReport(method, params) : partial(method, params)
      }
      return partial(method, params)
    })
    const { answers, load } = useOverviewData()

    await load('/volume1')
    expect(answers['/volume1'].overview?.results).toHaveLength(0)

    await load('/volume1')

    expect(overviewReads).toBe(2)
    expect(answers['/volume1'].overview?.results).toHaveLength(1)
  })

  it('reads again when the answers are invalidated, though it has a report', async () => {
    const base = fixtureBackend()
    let overviewReads = 0
    setBackend(async (method, params) => {
      if (method === 'overview.get') overviewReads += 1
      return base(method, params)
    })
    const { answers, load } = useOverviewData()

    await load('/volume1')
    await load('/volume1')
    expect(overviewReads).toBe(1)
    expect(answers['/volume1'].overview?.results).toHaveLength(1)

    invalidateAnswers()
    await load('/volume1')

    expect(overviewReads).toBe(2)
    expect(answers['/volume1'].overview?.results).toHaveLength(1)
  })

  it('publishes the overview while why-not-freed is still pending', async () => {
    const base = fixtureBackend()
    let releaseWhy = () => {}
    const whyGate = new Promise<void>(resolve => (releaseWhy = resolve))
    setBackend(async (method, params) => {
      if (method === 'why_not_freed.get') await whyGate
      return base(method, params)
    })
    const { answers, load } = useOverviewData()

    const pending = load('/volume1', { why: true })
    await vi.waitFor(() => expect(answers['/volume1'].overview?.results).toHaveLength(1))

    // The hero and cards are ready; only the optional explanations still load.
    expect(answers['/volume1'].overviewLoading).toBe(false)
    expect(answers['/volume1'].whyPending).toBe(true)
    expect(answers['/volume1'].why).toBeNull()
    expect(answers['/volume1'].error).toBeNull()

    releaseWhy()
    await pending
    expect(answers['/volume1'].whyPending).toBe(false)
    expect(answers['/volume1'].why?.results).toHaveLength(1)
  })

  it('keeps the overview answer when why-not-freed fails', async () => {
    const base = fixtureBackend()
    setBackend(async (method, params) => {
      if (method === 'why_not_freed.get') throw new ApiError('cli_error')
      return base(method, params)
    })
    const { answers, load } = useOverviewData()

    await load('/volume1', { why: true })

    expect(answers['/volume1'].overview?.results).toHaveLength(1)
    expect(answers['/volume1'].error).toBeNull()
    expect(answers['/volume1'].why).toBeNull()
    // The optional answer failed: the reasons stay unknown, not empty.
    expect(answers['/volume1'].whyPending).toBe(false)
    expect(answers['/volume1'].whyFailed).toBe(true)
  })

  it('still shows an overview failure as the answer error', async () => {
    const base = fixtureBackend()
    setBackend(async (method, params) => {
      if (method === 'overview.get') throw new ApiError('cli_error')
      return base(method, params)
    })
    const { answers, load } = useOverviewData()

    await load('/volume1', { why: true })

    expect(answers['/volume1'].overview).toBeNull()
    expect(answers['/volume1'].overviewLoading).toBe(false)
    expect(answers['/volume1'].error?.code).toBe('cli_error')
  })

  it('starts a fresh read when the answers version rises mid-flight and drops the old reply', async () => {
    const noReport = fixtureBackend({ scenario: 'no_report' })
    const partial = fixtureBackend({ scenario: 'partial' })
    let releaseStale = () => {}
    const staleGate = new Promise<void>(resolve => (releaseStale = resolve))
    let overviewReads = 0
    setBackend(async (method, params) => {
      if (method !== 'overview.get') return partial(method, params)
      overviewReads += 1
      if (overviewReads === 1) {
        await staleGate
        return noReport(method, params)
      }
      return partial(method, params)
    })
    const { answers, load } = useOverviewData()

    const stale = load('/volume1')
    await flush()
    invalidateAnswers()
    const fresh = load('/volume1')
    releaseStale()
    await Promise.all([stale, fresh])

    expect(overviewReads).toBe(2)
    expect(answers['/volume1'].version).toBe(answersVersion.value)
    // The reply from before the version change must not replace the fresh report.
    expect(answers['/volume1'].overview?.results).toHaveLength(1)
  })

  it('files a late answer under its own volume, never under the shown one', async () => {
    const base = fixtureBackend()
    const noReport = fixtureBackend({ scenario: 'no_report' })
    // The recorded report is keyed on /volume1; reuse it for the second volume.
    const report = await base('overview.get', { volume: '/volume1' })
    let releaseVolume1 = () => {}
    const volume1Gate = new Promise<void>(resolve => (releaseVolume1 = resolve))
    setBackend(async (method, params) => {
      if (method !== 'overview.get') return base(method, params)
      if ((params as { volume: string }).volume === '/volume1') {
        await volume1Gate
        return noReport(method, params)
      }
      return report
    })
    const { answers, load } = useOverviewData()

    const left = load('/volume1')
    await flush()
    await load('/volume2')
    releaseVolume1()
    await left

    expect(answers['/volume2'].overview?.results).toHaveLength(1)
    expect(answers['/volume1'].overview?.results).toHaveLength(0)
  })
})
