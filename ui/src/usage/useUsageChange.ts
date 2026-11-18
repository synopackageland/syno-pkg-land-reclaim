// One visible-window sampler shared by Overview and Explore. No scan starts here.
import { computed, reactive, ref, watch, type Ref } from 'vue'
import { api } from '@/api'
import { jobs, runningJobs } from '@/stores/jobs'
import { selectedVolume, volumes } from '@/stores/volumes'
import { baselineFor, dismiss, sample, snooze, SAMPLE_INTERVAL, type AlertRecord } from './usageChange'

export interface UsageAlert { key: string; mount: string; used: number; delta: number }
export const usageChange = reactive({ alerts: [] as UsageAlert[], unavailable: [] as string[] })
let records: Record<string, AlertRecord> = {}

/** The banner shows while no job runs; the no-baseline hint names the selected volume. */
export const usageAlertShown = computed(() => !runningJobs.value.length && usageChange.alerts.length > 0)
export const usageBaselineMissing = computed(() => !!selectedVolume.value && usageChange.unavailable.includes(selectedVolume.value.mount.path))

function keyFor(mount: string, fsid: string) {
  return `synoreclaim.usage.v1:${encodeURIComponent(mount)}:${fsid}`
}

function read(key: string): AlertRecord | undefined {
  try {
    const value = JSON.parse(localStorage.getItem(key) ?? 'null') as AlertRecord | null
    if (value && typeof value.baselineId === 'string' && typeof value.visible === 'boolean'
      && typeof value.crossed === 'boolean' && Number.isFinite(value.sampledAt)
      && [value.lastAlert, value.snoozedUntil, value.dismissedUsed].every(v => v === null || (typeof v === 'number' && Number.isFinite(v)))) return value
  } catch { /* Blocked or damaged storage: continue in memory. */ }
  return records[key]
}

function save(key: string, record: AlertRecord) {
  records[key] = record
  try { localStorage.setItem(key, JSON.stringify(record)) } catch { /* In-memory fallback. */ }
}

export async function actOnAlerts(action: 'snooze' | 'dismiss') {
  const alerts = [...usageChange.alerts]
  const previousVolumes = volumes.document?.volumes ?? []
  for (const alert of alerts) {
    const state = records[alert.key]
    if (state) save(alert.key, action === 'snooze' ? snooze(state, Date.now()) : dismiss(state, alert.used))
  }
  usageChange.alerts = []
  if (action !== 'dismiss' || document.hidden || runningJobs.value.length) return
  try {
    const current = await api('volumes.list', {})
    if (document.hidden || runningJobs.value.length) return
    for (const alert of alerts) {
      const volume = current.volumes.find(volume => volume.fsid && keyFor(volume.mount.path, volume.fsid) === alert.key)
      const previous = previousVolumes.find(volume => volume.mount.path === alert.mount)
      const state = read(alert.key)
      if (volume && previous && volume.total_bytes === previous.total_bytes
        && volume.used_bytes != null && Number.isSafeInteger(volume.used_bytes) && volume.used_bytes >= 0
        && state && !state.visible && state.dismissedUsed === alert.used) save(alert.key, dismiss(state, volume.used_bytes))
    }
  } catch { /* Already dismissed with the last known reading; keep it closed if a refresh fails. */ }
}

/** App owns the lifetime; changing pages does not restart the sampling interval. */
export function startUsageSampling(appVisible: Ref<boolean> = ref(true), { firstDelayMs = 0 }: { firstDelayMs?: number } = {}) {
  records = {}
  usageChange.alerts = []
  usageChange.unavailable = []
  let timer: ReturnType<typeof setTimeout> | undefined
  let stopped = false
  let inFlight = false
  let deferred = false
  let generation = 0
  const canSample = () => !stopped && !document.hidden && appVisible.value && !runningJobs.value.length && !jobs.error

  async function takeSample() {
    clearTimeout(timer)
    if (stopped || document.hidden || !appVisible.value) return
    if (!canSample()) { deferred = true; return }
    if (inFlight) return
    inFlight = true
    deferred = false
    const requestGeneration = generation
    const stillCurrent = () => canSample() && generation === requestGeneration
    try {
      const current = await api('volumes.list', {})
      if (!stillCurrent()) { deferred = true; return }
      const history = await api('history.list', {})
      if (!stillCurrent()) { deferred = true; return }
      const alerts: UsageAlert[] = []
      const unavailable: string[] = []
      for (const volume of current.volumes) {
        const baseline = baselineFor(volume, history.scans)
        if (!baseline) { unavailable.push(volume.mount.path); continue }
        const key = keyFor(volume.mount.path, baseline.fsid)
        const used = volume.used_bytes!
        const state = sample(baseline, used, read(key), Date.now())
        save(key, state)
        if (state.visible) alerts.push({ key, mount: volume.mount.path, used, delta: used - baseline.used })
      }
      volumes.document = current
      usageChange.alerts = alerts
      usageChange.unavailable = unavailable
    } catch {
      // A failed request is not a sample and cannot confirm a crossing.
      if (!stopped) usageChange.alerts = []
    } finally {
      inFlight = false
      if (canSample() && deferred) void takeSample()
      else if (!stopped && !document.hidden && appVisible.value) timer = setTimeout(() => { void takeSample() }, SAMPLE_INTERVAL)
    }
  }

  function visibilityChanged() {
    generation += 1
    deferred = true
    clearTimeout(timer)
    if (!document.hidden && appVisible.value) void takeSample()
  }
  const stopWatch = watch(() => [runningJobs.value.length, jobs.error] as const, ([count, error], previous) => {
    if (previous && previous[0] === count && previous[1] === error) return
    generation += 1
    if (count || error) deferred = true
    else if (deferred) void takeSample()
  }, { flush: 'sync' })
  const stopVisibilityWatch = watch(appVisible, visibilityChanged)
  document.addEventListener('visibilitychange', visibilityChanged)
  if (firstDelayMs > 0) timer = setTimeout(() => { void takeSample() }, firstDelayMs)
  else void takeSample()
  return () => {
    stopped = true
    clearTimeout(timer)
    stopWatch()
    stopVisibilityWatch()
    document.removeEventListener('visibilitychange', visibilityChanged)
  }
}
