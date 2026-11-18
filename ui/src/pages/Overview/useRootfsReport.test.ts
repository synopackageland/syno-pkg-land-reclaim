import { flushPromises } from '@vue/test-utils'
import { afterEach } from 'vitest'
import { effectScope, type EffectScope } from 'vue'
import { setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import { fixtureBackend } from '@/api/fixtures'
import { invalidateAnswers } from '@/stores/answers'
import { REFRESH_AFTER_S, SAVED_MAX_AGE_S, useRootfsReport as useInScope } from './useRootfsReport'

// Each view in its own scope, stopped after its test, as an unmounted view's is.
const scopes: EffectScope[] = []
function useRootfsReport() {
  const scope = effectScope()
  scopes.push(scope)
  return scope.run(useInScope)!
}

afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  setBackend(fixtureBackend())
  vi.useRealTimers()
})

const NOW = Date.parse('2026-10-31T12:00:00Z')
const report = (ageS: number, id = 'saved') => ({
  command: 'rootfs',
  completion: 'complete',
  generated_at: new Date(NOW - ageS * 1000).toISOString(),
  errors: [],
  warnings: [],
  owners: [],
  id,
})

/**
 * The CLI: a call with max_age_s answers the saved report at once when it
 * has one; otherwise, and with max_age_s 0, it queues a walk whose result
 * `fresh` is.
 */
function cli({ saved, fresh = report(0, 'fresh') }: { saved: ReturnType<typeof report> | null; fresh?: ReturnType<typeof report> }) {
  const calls: unknown[] = []
  let finish: () => void = () => {}
  const walked = new Promise<void>(resolve => { finish = resolve })
  const backend = (async (method: string, params: { max_age_s?: number; job_id?: string }) => {
    if (method === 'rootfs.report') {
      calls.push(params)
      if ((params.max_age_s ?? 0) > 0 && saved) return saved
      return { command: 'detach', job_id: 'walk-1' }
    }
    if (method === 'jobs.list') {
      await walked
      return { jobs: [{ job_id: 'walk-1', kind: 'rootfs-report', state: 'completed' }] }
    }
    if (method === 'job.result') return fresh
    throw new Error(`unexpected ${method}`)
  }) as unknown as Backend
  setBackend(backend)
  return { calls, finish }
}

describe('useRootfsReport', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(NOW)
  })

  it('shows the saved report at once and swaps in a fresh one checked in the background', async () => {
    const { calls, finish } = cli({ saved: report(REFRESH_AFTER_S + 1) })
    const { report: shown, loading, refreshing, load } = useRootfsReport()

    await load()

    expect(shown.value).toMatchObject({ id: 'saved' })
    expect(loading.value).toBe(false)
    expect(refreshing.value).toBe(true)
    finish()
    await vi.waitFor(() => expect(refreshing.value).toBe(false))
    expect(shown.value).toMatchObject({ id: 'fresh' })
    expect(calls).toEqual([{ max_age_s: SAVED_MAX_AGE_S }, { max_age_s: 0 }])
  })

  it('does not walk again when the saved report is recent', async () => {
    const { calls } = cli({ saved: report(60) })
    const { report: shown, refreshing, load } = useRootfsReport()

    await load()
    await flushPromises()

    expect(shown.value).toMatchObject({ id: 'saved' })
    expect(refreshing.value).toBe(false)
    expect(calls).toEqual([{ max_age_s: SAVED_MAX_AGE_S }])
  })

  it('walks with the loading state only when nothing is saved', async () => {
    const { calls, finish } = cli({ saved: null })
    const { report: shown, loading, refreshing, load } = useRootfsReport()

    const reading = load()
    await flushPromises()
    expect(loading.value).toBe(true)
    expect(refreshing.value).toBe(false)
    finish()
    await reading

    expect(shown.value).toMatchObject({ id: 'fresh' })
    expect(loading.value).toBe(false)
    expect(calls).toEqual([{ max_age_s: SAVED_MAX_AGE_S }])
  })

  it('keeps the shown report while checking again on request', async () => {
    const { calls, finish } = cli({ saved: report(60) })
    const { report: shown, refreshing, load } = useRootfsReport()
    await load()

    const again = load({ force: true })
    await flushPromises()
    expect(shown.value).toMatchObject({ id: 'saved' })
    expect(refreshing.value).toBe(true)
    finish()
    await again

    expect(shown.value).toMatchObject({ id: 'fresh' })
    expect(refreshing.value).toBe(false)
    expect(calls).toEqual([{ max_age_s: SAVED_MAX_AGE_S }, { max_age_s: 0 }])
  })

  it('keeps the shown report and says so when checking again fails', async () => {
    cli({ saved: report(60) })
    const { report: shown, error, refreshing, load } = useRootfsReport()
    await load()
    setBackend((async () => { throw new Error('down') }) as unknown as Backend)

    await load({ force: true })

    expect(shown.value).toMatchObject({ id: 'saved' })
    expect(refreshing.value).toBe(false)
    expect(error.value?.code).toBe('unknown')
  })

  it('walks again, without the saved report, when a rootfs apply invalidates the answers', async () => {
    const { calls, finish } = cli({ saved: report(60) })
    const { report: shown, load } = useRootfsReport()
    await load()

    invalidateAnswers()
    finish()
    await vi.waitFor(() => expect(shown.value).toMatchObject({ id: 'fresh' }))

    expect(calls).toEqual([{ max_age_s: SAVED_MAX_AGE_S }, { max_age_s: 0 }])
  })
})

it('opens the requested saved report without starting another system scan', async () => {
  const backend = fixtureBackend({ scenario: 'sysvol_full' })
  const accepted = await backend('rootfs.report', {})
  const calls: string[] = []
  setBackend((method, params) => { calls.push(method); return backend(method, params) })
  const view = useRootfsReport()
  const reading = view.loadResult(accepted.job_id!)
  await view.load()
  await reading
  expect(view.report.value?.command).toBe('rootfs')
  expect(calls).toEqual(['job.result'])
})
