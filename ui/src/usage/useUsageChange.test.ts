import { flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import type { Backend } from '@/api/methods'
import { jobs } from '@/stores/jobs'
import type { Job } from '@/types'
import { startUsageSampling, usageChange, actOnAlerts } from './useUsageChange'
import { FIRST_SAMPLE_DELAY, SAMPLE_INTERVAL } from './usageChange'

let stop: (() => void) | undefined
let reads: string[]
let hidden = false
let used = 450e9
const normal = fixtureBackend({ scenario: 'interaction' })
const backend: Backend = async (method, params) => {
  reads.push(method)
  const result = await normal(method, params)
  if (method === 'volumes.list') {
    const doc = JSON.parse(JSON.stringify(result)) as import('@/types').VolumesDocument
    doc.volumes = [doc.volumes[0]]
    Object.assign(doc.volumes[0], { fsid: 'fs1', total_bytes: 1e12, used_bytes: used })
    return doc as never
  }
  if (method === 'history.list') {
    const doc = JSON.parse(JSON.stringify(result)) as import('@/types').HistoryDocument
    doc.scans = [doc.scans[1]]
    Object.assign(doc.scans[0], { fsid: 'fs1', df_total_bytes: 1e12, df_used_bytes: 400e9 })
    return doc as never
  }
  return result
}

beforeEach(async () => {
  await normal('volumes.list', {})
  await normal('history.list', {})
  vi.useFakeTimers()
  vi.setSystemTime(new Date('2026-10-01T00:00:00Z'))
  localStorage.clear()
  jobs.list = []
  jobs.error = null
  hidden = false
  used = 450e9
  reads = []
  vi.spyOn(document, 'hidden', 'get').mockImplementation(() => hidden)
  setBackend(backend)
})
afterEach(() => { stop?.(); stop = undefined; vi.restoreAllMocks(); vi.useRealTimers() })

it('samples on open and every 15 minutes, persisting dismissal across windows', async () => {
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(0)
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL)
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(1)
  await actOnAlerts('dismiss')
  stop()
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(0)
  expect(reads.filter(method => method === 'scan.start')).toHaveLength(0)
})

it('stops while hidden and samples immediately on foreground return', async () => {
  hidden = true
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toHaveLength(0)
  hidden = false
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toEqual(['volumes.list', 'history.list'])
  hidden = true
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL * 2)
  expect(reads).toHaveLength(2)
  hidden = false
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(1)
})

it('defers until all active jobs end, including cleanup jobs', async () => {
  jobs.list = [{ job_id: 'cleanup', kind: 'cleanup-apply', state: 'running' } as Job]
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toHaveLength(0)
  jobs.list = []
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toHaveLength(2)
})

it('keeps persistence failures from breaking sampling or dismissal', async () => {
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked') })
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL)
  await vi.dynamicImportSettled()
  await flushPromises()
  await actOnAlerts('dismiss')
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL)
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(0)
})

it('does not publish requests that finish after hiding or closing the app', async () => {
  let release!: () => void
  const wait = new Promise<void>(resolve => { release = resolve })
  setBackend(async (method, params) => { await wait; return backend(method, params) })
  stop = startUsageSampling()
  stop()
  release()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(0)
  expect(reads).toEqual(['volumes.list'])
})

it('also suspends sampling while the DSM app window is minimized', async () => {
  const { ref } = await import('vue')
  const visible = ref(false)
  stop = startUsageSampling(visible)
  await flushPromises()
  expect(reads).toHaveLength(0)
  visible.value = true
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toHaveLength(2)
  visible.value = false
  await flushPromises()
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL * 2)
  expect(reads).toHaveLength(2)
})

it('records a fresh df used reading when Close is clicked, rather than the 15-minute-old sample', async () => {
  stop = startUsageSampling()
  await vi.dynamicImportSettled()
  await flushPromises()
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL)
  await vi.dynamicImportSettled()
  await flushPromises()
  used = 480e9
  const closing = actOnAlerts('dismiss')
  await vi.dynamicImportSettled()
  await closing
  const state = JSON.parse(localStorage.getItem('synoreclaim.usage.v1:%2Fvolume1:fs1')!)
  expect(state.dismissedUsed).toBe(480e9)
})

it('discards a sample that spans a hide/foreground cycle and takes a fresh foreground sample', async () => {
  let release!: () => void
  const wait = new Promise<void>(resolve => { release = resolve })
  let first = true
  setBackend(async (method, params) => {
    if (method === 'volumes.list' && first) { first = false; await wait }
    return backend(method, params)
  })
  stop = startUsageSampling()
  hidden = true
  document.dispatchEvent(new Event('visibilitychange'))
  hidden = false
  document.dispatchEvent(new Event('visibilitychange'))
  release()
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toEqual(['volumes.list', 'volumes.list', 'history.list'])
  expect(usageChange.alerts).toHaveLength(0)
  await vi.advanceTimersByTimeAsync(SAMPLE_INTERVAL)
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(usageChange.alerts).toHaveLength(1)
})

it('leaves the NAS to the first page for firstDelayMs, then samples', async () => {
  stop = startUsageSampling(undefined, { firstDelayMs: FIRST_SAMPLE_DELAY })
  await vi.advanceTimersByTimeAsync(FIRST_SAMPLE_DELAY - 1)
  expect(reads).toHaveLength(0)
  await vi.advanceTimersByTimeAsync(1)
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toEqual(['volumes.list', 'history.list'])
})

it('samples at once on a foreground return even before the first delay is up', async () => {
  stop = startUsageSampling(undefined, { firstDelayMs: FIRST_SAMPLE_DELAY })
  hidden = true
  document.dispatchEvent(new Event('visibilitychange'))
  hidden = false
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.dynamicImportSettled()
  await flushPromises()
  expect(reads).toEqual(['volumes.list', 'history.list'])
  await vi.advanceTimersByTimeAsync(FIRST_SAMPLE_DELAY)
  expect(reads).toEqual(['volumes.list', 'history.list'])
})
