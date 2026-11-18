import type { HistoryDocument } from '@/types'
import { coversPath, reportScope, reportsCovering, type ScanRecord } from './reportChoice'

const scan = (job_id: string, finished: string, targets: string[], extra: Partial<ScanRecord> = {}) =>
  ({ job_id, finished, volume: false, outcome: 'completed', report: { path: `/state/${job_id}` }, targets: targets.map(path => ({ path })), ...extra }) as ScanRecord

describe('report choice', () => {
  const volume = scan('vol', '2026-10-30T00:00:00Z', ['/volume1'], { volume: true })
  const share = scan('share', '2026-11-05T00:00:00Z', ['/volume1/fixture'])
  const folder = scan('folder', '2026-11-04T00:00:00Z', ['/volume1/fixture/a', '/volume1/fixture/b'])
  const superseded = scan('old', '2026-10-01T00:00:00Z', ['/volume1/fixture'], { outcome: 'superseded', report: null } as unknown as Partial<ScanRecord>)
  const other = scan('other', '2026-11-06T00:00:00Z', ['/volume2'], { volume: true })
  const history = { scans: [volume, share, folder, superseded, other] } as unknown as HistoryDocument

  it('says which saved reports cover a path, newest first', () => {
    expect(reportsCovering(history, '/volume1', '/volume1').map(s => s.job_id)).toEqual(['vol'])
    expect(reportsCovering(history, '/volume1', '/volume1/fixture').map(s => s.job_id)).toEqual(['share', 'vol'])
    expect(reportsCovering(history, '/volume1', '/volume1/fixture/a/x').map(s => s.job_id)).toEqual(['share', 'folder', 'vol'])
    expect(reportsCovering(history, '/volume1', '/volume1/fixture2').map(s => s.job_id)).toEqual(['vol'])
    expect(reportsCovering(null, '/volume1', '/volume1')).toEqual([])
  })

  it('never takes a sibling with a longer name as covered', () => {
    expect(coversPath(share, '/volume1', '/volume1/fixture-2')).toBe(false)
    expect(coversPath(volume, '/volume1', '/volume10/x')).toBe(false)
  })

  it('names what a scan read', () => {
    expect(reportScope(volume, '/volume1')).toEqual({ key: 'volume' })
    expect(reportScope(share, '/volume1')).toEqual({ key: 'share', name: 'fixture' })
    expect(reportScope(folder, '/volume1')).toEqual({ key: 'paths', count: 2 })
    expect(reportScope(scan('deep', '', ['/volume1/fixture/a']), '/volume1')).toEqual({ key: 'paths', count: 1 })
  })
})
