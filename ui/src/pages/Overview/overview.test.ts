import overviewDoc from '../../../fixtures/overview.get/interaction.json'
import partialDoc from '../../../fixtures/overview.get/volume1-partial.json'
import realVolumeDoc from '../../../fixtures/overview.get/volume1.json'
import whyDoc from '../../../fixtures/why_not_freed.get/interaction.json'
import pausedJobs from '../../../fixtures/jobs.list/paused.json'
import runningJobs from '../../../fixtures/jobs.list/interaction-running.json'
import volumesDoc from '../../../fixtures/volumes.list/interaction.json'
import type { Job, OverviewDocument, Volume, WhyNotFreedDocument } from '@/types'
import { actionItems, breakdown, byFolder, byPackage, chartItems, volumeStatus, type BreakdownItem, type OverviewResult } from './overview'

const result = (overviewDoc as OverviewDocument).results[0]
const partial = (partialDoc as OverviewDocument).results[0]
const realVolume = (realVolumeDoc as OverviewDocument).results[0]
const reasons = (whyDoc as WhyNotFreedDocument).results[0].reasons
const volume = (volumesDoc as { volumes: Volume[] }).volumes[0]

function withParts(patch: (r: OverviewResult) => void): OverviewResult {
  const copy = structuredClone(result)
  patch(copy)
  return copy
}
const part = (r: OverviewResult, kind: string) => r.parts.find(p => p.kind === kind)!

describe('folder donut', () => {
  it('by folder keeps the seven largest shares and groups the rest', () => {
    const r = withParts(r => {
      part(r, 'share_live').items = Array.from({ length: 9 }, (_, i) => ({ share: `s${i}`, bytes: i + 1, reclaimable_bytes: 0 }))
    })
    const segments = byFolder(r)
    expect(segments.map(s => s.label)).toEqual(['s8', 's7', 's6', 's5', 's4', 's3', 's2', undefined])
    expect(segments.at(-1)).toMatchObject({ key: 'other', value: 3 })
  })
})

describe('breakdown', () => {
  const inBreakdown = (items: BreakdownItem[]) => items.filter(i => i.inBreakdown)
  const sum = (items: { bytes: number }[]) => items.reduce((total, i) => total + i.bytes, 0)

  it('adds up to the volume size, once, in the model and in the chart', () => {
    for (const r of [result, partial, realVolume]) {
      const items = breakdown(r, null)
      expect(sum(inBreakdown(items))).toBe(r.space.df!.total_bytes)
      expect(sum(chartItems(items))).toBe(r.space.df!.total_bytes)
      expect(sum(chartItems(items).filter(i => i.category !== 'free'))).toBe(r.space.df!.used_bytes)
    }
  })

  it('keeps the report parts aligned with the reconciliation total and puts the df remainder apart', () => {
    const items = inBreakdown(breakdown(realVolume, null))
    const parts = items.filter(i => !['other_used', 'free'].includes(i.category))
    expect(sum(parts)).toBe(realVolume.reconciliation.data.parts_sum_bytes)
    expect(items.find(i => i.category === 'other_used')!.bytes).toBe(realVolume.space.df!.used_bytes - sum(parts))
  })

  it('lists the parts in chart order, without empty ones', () => {
    expect(chartItems(breakdown(result, null)).map(i => [i.key, i.bytes])).toEqual([
      ['share_live', 2125824],
      ['dsm_service:synology_drive', 262144],
      ['dsm_service:active_backup_data', 131072],
      ['dsm_service:container_manager', 131072],
      ['dsm_service:surveillance_recordings', 131072],
      ['dsm_service:hyper_backup', 131072],
      ['dsm_service:other', 131072],
      ['recycle_bin', 196608],
      ['pinned_unreferenced', 65536],
      ['other_used', 6750208],
      ['free', 526815232],
    ])
  })

  it('splits DSM service data into its large services and groups the small ones', () => {
    const items = breakdown(realVolume, null)
    const dsm = chartItems(items).filter(i => i.category === 'dsm_service')
    expect(dsm.map(i => i.key)).toEqual(['dsm_service:container_manager', 'dsm_service:packages', 'dsm_service:iscsi_luns', 'dsm_service:other'])
    expect(sum(dsm)).toBe(part(realVolume, 'dsm_service').bytes)
    // Less than 1% of the used space, so in the group: Synology Drive (403 MB of 43 GB).
    expect(dsm.map(i => i.service)).not.toContain('synology_drive')
    expect(dsm.at(-1)).toMatchObject({ sub: 'other', action: 'packages' })
  })

  it('names the report’s own "other DSM data" only in the group', () => {
    const items = breakdown(withParts(r => {
      part(r, 'dsm_service').items!.find(i => i.service === 'other_dsm_service_data')!.bytes = 9_000_000
      part(r, 'dsm_service').bytes += 9_000_000 - 65536
    }), null)
    expect(items.map(i => i.service)).not.toContain('other_dsm_service_data')
    expect(items.find(i => i.key === 'dsm_service:other')!.bytes).toBeGreaterThanOrEqual(9_000_000)
  })

  it('keeps DSM service data whole when no service is large enough, or the report has no services', () => {
    const none = breakdown(withParts(r => { part(r, 'dsm_service').items = [] }), null)
    expect(none.filter(i => i.category === 'dsm_service')).toEqual([expect.objectContaining({ key: 'dsm_service', bytes: 917504, action: 'packages' })])
  })

  it('gives the shown chart part and its list row the same size and bound', () => {
    for (const r of [result, partial, realVolume]) {
      const items = breakdown(r, null)
      const chart = new Map(chartItems(items).map(i => [i.key, i]))
      for (const row of actionItems(items).filter(i => chart.has(i.key))) {
        expect({ key: row.key, bytes: row.bytes, bound: row.bound }).toEqual({ key: row.key, bytes: chart.get(row.key)!.bytes, bound: chart.get(row.key)!.bound })
      }
    }
  })

  it('says snapshots hold at least their size while some were not read, and exactly it otherwise', () => {
    const r = withParts(r => { part(r, 'snapshot_only').bytes = 4096; part(r, 'snapshot_only').items = [{ share: 'share04', bytes: 4096, reclaimable_bytes: null }] })
    const snapshot = (x: OverviewResult) => breakdown(x, null).filter(i => i.category === 'snapshot_only')
    expect(snapshot(r)).toEqual([expect.objectContaining({ key: 'snapshot_only', bytes: 4096, bound: 'at_least', action: 'snapshot', shares: ['share04'] })])
    r.report.coverage.snapshots_not_materialized = 0
    expect(snapshot(r)[0].bound).toBe('exact')
  })

  it('lists large paths inside shared-folder data, without counting them twice', () => {
    const items = breakdown(realVolume, null)
    const cleanup = items.filter(i => i.action === 'cleanup')
    expect(cleanup.map(i => [i.path, i.bytes, i.bound])).toEqual([
      ['/volume1/share62/folder0176', 13717508096, 'exact'],
      ['/volume1/share50/folder0141', 1514835968, 'exact'],
      ['/volume1/share62/folder0213', 26501120, 'exact'],
    ])
    expect(sum(items.filter(i => i.category === 'share_live'))).toBe(part(realVolume, 'share_live').bytes)
    expect(chartItems(items).find(i => i.key === 'share_live')!.bytes).toBe(part(realVolume, 'share_live').bytes)
  })

  it('has one Recycle Bin item with its shares, largest first', () => {
    const recycle = breakdown(realVolume, null).filter(i => i.category === 'recycle_bin')
    expect(recycle).toEqual([expect.objectContaining({ bytes: part(realVolume, 'recycle_bin').bytes, bound: 'exact', action: 'recycle' })])
    expect(recycle[0].shares.slice(0, 2)).toEqual(['share50', 'share59'])
  })

  it('has one background cleanup item, with the subvolume count of why-not-freed', () => {
    const why = structuredClone(reasons)
    Object.assign(why.find(x => x.code === 'pending_subvolume_cleanup')!, { count: 2, bytes: 5000 })
    const items = breakdown(withParts(r => { part(r, 'pending_cleanup').bytes = 5000 }), why)
    expect(items.filter(i => i.category === 'pending_cleanup')).toEqual([expect.objectContaining({ bytes: 5000, action: 'wait', count: 2, bound: 'exact' })])
  })

  it('adds deleted-but-open files from why-not-freed as a row outside the breakdown', () => {
    const why = structuredClone(reasons)
    Object.assign(why.find(x => x.code === 'open_unlinked')!, { report_orphan_bytes: 4096 })
    const open = breakdown(result, why).find(i => i.action === 'open_unlinked')!
    expect(open).toMatchObject({ bytes: 4096, bound: 'exact', inBreakdown: false })
    Object.assign(why.find(x => x.code === 'open_unlinked')!, { processes: { bytes_estimate: 8192, processes: [], unreadable_processes: 0 } })
    expect(breakdown(result, why).find(i => i.action === 'open_unlinked')).toMatchObject({ bytes: 8192, bound: 'up_to' })
    expect(chartItems(breakdown(result, why)).map(i => i.key)).not.toContain('open_unlinked')
  })

  it('offers how to resolve unknown data, but not compact coverage noise', () => {
    expect(breakdown(realVolume, null).find(i => i.category === 'unknown')).toMatchObject({ key: 'unknown', action: 'unresolved_references' })
  })

  it('suggests the items with an action, largest first', () => {
    const rows = actionItems(breakdown(realVolume, null))
    expect(rows.every(row => row.action)).toBe(true)
    expect(rows.map(row => row.bytes)).toEqual([...rows.map(row => row.bytes)].sort((a, b) => b - a))
    expect(rows.map(row => row.key)).toEqual(expect.arrayContaining(['dsm_service:iscsi_luns', 'dsm_service:container_manager', 'recycle_bin', 'snapshot_only', 'unknown']))
    expect(rows.map(row => row.key)).not.toContain('free')
  })
})

describe('volume status', () => {
  const jobsOf = (doc: unknown) => (doc as { jobs: Job[] }).jobs

  it('is the last scan when no job covers the volume', () => {
    expect(volumeStatus(volume, [])).toEqual({ kind: 'scanned', at: '2026-09-29T10:58:31Z', consistency: 'observed-stable' })
  })

  it('prefers the loaded report over the volume list', () => {
    expect(volumeStatus(volume, [], partial.report)).toEqual({ kind: 'scanned', at: '2026-09-29T02:29:26Z', consistency: 'partial' })
  })

  it('is no_report before the first scan', () => {
    expect(volumeStatus({ ...volume, reports: 0, last_scan: null }, [])).toEqual({ kind: 'no_report' })
  })

  it('shows a paused job with its progress', () => {
    const status = volumeStatus(volume, jobsOf(pausedJobs), undefined, Date.parse('2026-09-30T01:04:20Z'))
    expect(status.kind).toBe('paused')
    expect(status.kind === 'paused' && status.percent).toBeCloseTo(4200000 / 12000000)
  })

  it('hides a paused job older than seven days (D-4)', () => {
    const paused = jobsOf(pausedJobs)
    const eightDaysLater = Date.parse(paused[0].updated) + 8 * 24 * 60 * 60 * 1000
    expect(volumeStatus(volume, paused, undefined, eightDaysLater).kind).toBe('scanned')
  })

  it('has no percentage without the last scan as a basis, even with units counted', () => {
    const [job] = jobsOf(runningJobs)
    const status = volumeStatus(volume, [{ ...job, progress: { ...job.progress!, basis: null } }])
    expect(status.kind === 'scanning' && status.percent).toBeNull()
  })

  it('shows a running job', () => {
    expect(volumeStatus(volume, jobsOf(runningJobs)).kind).toBe('scanning')
  })
})

 it('does not label a cleanup apply as a scan on the volume card', () => {
  const job = (runningJobs as unknown as { jobs: Job[] }).jobs[0]!
  expect(volumeStatus(volume, [{ ...job, kind: 'cleanup-apply' }]).kind).toBe('scanned')
})

describe('packages donut', () => {
  const items = (n: number) => Array.from({ length: n }, (_, i) => ({ service: `pkg${i}`, managed_by: `Package ${i}`, manage_in_id: 'package_center', bytes: (i + 1) * 100 }))

  it('keeps the seven largest packages, largest first, and groups the rest', () => {
    const segments = byPackage(withParts(r => { part(r, 'dsm_service').items = items(9) as never }))
    expect(segments.map(s => s.service)).toEqual(['pkg8', 'pkg7', 'pkg6', 'pkg5', 'pkg4', 'pkg3', 'pkg2', undefined])
    expect(segments[0]).toMatchObject({ key: 'package:pkg8', value: 900, label: 'Package 8' })
    expect(segments.at(-1)).toMatchObject({ key: 'other', value: 300 })
  })

  it('adds up to all package data of the DSM service data part', () => {
    const all = (part(realVolume, 'dsm_service').items as { bytes: number }[]).reduce((sum, i) => sum + i.bytes, 0)
    const shown = byPackage(realVolume).reduce((sum, s) => sum + s.value, 0)
    expect(shown).toBeGreaterThan(0)
    expect(shown).toBeLessThanOrEqual(all)
  })

  it('is empty without a DSM service data part or without items', () => {
    expect(byPackage(withParts(r => { r.parts = r.parts.filter(p => p.kind !== 'dsm_service') }))).toEqual([])
    expect(byPackage(withParts(r => { part(r, 'dsm_service').items = [] }))).toEqual([])
  })

  it('includes package data kept in shared folders, as the Packages page does', () => {
    const segments = byPackage(withParts(r => {
      part(r, 'dsm_service').items = [
        { service: 'surveillance_recordings', managed_by: 'Surveillance Station', manage_in_id: 'surveillance_station', bytes: 900 },
        { service: 'hyper_backup', managed_by: 'Hyper Backup', manage_in_id: 'hyper_backup', bytes: 800 },
        { service: 'container_manager', managed_by: 'Container Manager', manage_in_id: 'container_manager', bytes: 10 },
      ] as never
    }))
    expect(segments.map(s => s.service)).toEqual(['surveillance_recordings', 'hyper_backup', 'container_manager'])
  })
})
