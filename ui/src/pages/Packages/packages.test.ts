import type { TopDocument } from '@/types'
import { findRow, handling, howKey, LISTED_PATHS, packageRows } from './packages'

type Result = TopDocument['results'][number]
type Line = Result['services']['lines'][number]
type Entry = Result['entries'][number]

const metrics = (physical: number, extra: Partial<Line['metrics']> = {}): Line['metrics'] => ({
  logical: physical, physical, shared: 0, reclaimable: physical, reclaimable_resolved: physical,
  unknown: 0, pinned_unreferenced: 0, extents: 1, inline: 0, entries: 1, files: 1, ...extra,
})

const line = (service: string, physical: number, extra: Partial<Line> = {}): Line => ({
  service, names: `@${service}`, package: service, manage_in: 'x', manage_in_id: 'package_center',
  confirmed: true, paths: 1, metrics: metrics(physical), snapshots_not_read: 0, ...extra,
})

const entry = (service: string, path: string, extra: Record<string, unknown> = {}): Entry => ({
  path, kind: 'service', flags: ['system'], metrics: metrics(1), blockers: [], nlink: 1, tree: 256, objectid: 1,
  service: {
    id: service, names: `@${service}`, managed_by: service, manage_in: 'x', manage_in_id: 'package_center',
    confirmed: true, paths: [{ path }], path_count: 1,
  },
  cleanup_eligible: false,
  held_by: [],
  where_to_fix: { token: 'package', manage_in: 'x', manage_in_id: 'package_center', command: null },
  ...extra,
}) as Entry

const result = (lines: Line[], entries: Entry[] = []) =>
  ({ services: { lines, other_snapshots_not_read: 0 }, entries }) as Result

it('keeps folder metadata out of the package size ranking', () => {
  const rows = packageRows(result([line('folder_metadata', 100), line('drive', 5)]))
  expect(rows.map(row => row.service)).toEqual(['drive'])
})

describe('packageRows', () => {
  it('is empty without an answer or without service lines', () => {
    expect(packageRows(undefined)).toEqual([])
    expect(packageRows(result([]))).toEqual([])
  })

  it('lists the lines largest physical size first, whatever order the answer had', () => {
    const rows = packageRows(result([line('small', 10), line('large', 300), line('middle', 20)]))
    expect(rows.map(row => row.service)).toEqual(['large', 'middle', 'small'])
  })

  // On a test NAS, three empty VDISK_* lines read as three Virtual Machine Manager rows of 0 B.
  it('keeps one row of a package when its other lines are empty', () => {
    const rows = packageRows(result([
      line('vmm_virtual_disks', 0, { path: '/v/@iSCSI/LUN/VDISK_C2' }),
      line('vmm_virtual_disks', 300, { path: '/v/@iSCSI/LUN/VDISK_BLUN' }),
      line('vmm_virtual_disks', 0, { path: '/v/@iSCSI/LUN/VDISK_COW' }),
      line('usb_copy', 0, { path: '/v/@usbcopy' }),
      line('usb_copy', 0, { path: '/v/@usbcopy2' }),
      line('drive', 0, { path: '/v/a' }),
      line('drive', 7, { path: '/v/b' }),
    ]))
    expect(rows.map(row => [row.service, row.physical])).toEqual([
      ['vmm_virtual_disks', 300], ['drive', 7], ['usb_copy', 0],
    ])
  })

  it('leaves out the Recycle Bin lines, which the Recycle Bin page lists', () => {
    const rows = packageRows(result([line('recycle_bin', 50, { path: '/v/a/#recycle' }), line('docker', 10)]))
    expect(rows.map(row => row.service)).toEqual(['docker'])
  })

  it('carries the line values: size, can free, unknown, snapshots not read, place and confirmation', () => {
    const [row] = packageRows(result([
      line('abm', 100, {
        metrics: metrics(100, { reclaimable: null, unknown: 7 }),
        snapshots_not_read: 2, confirmed: false, manage_in_id: 'active_backup_business', paths: 3,
      }),
    ]))
    expect(row).toMatchObject({
      service: 'abm', physical: 100, canFree: null, unknown: 7, snapshotsNotRead: 2,
      confirmed: false, manageInId: 'active_backup_business', pathCount: 3,
    })
  })

  it('takes bytes held by snapshots from the service entry of the same path', () => {
    const rows = packageRows(result(
      [line('drive', 100, { path: '/v/@synologydrive' })],
      [entry('drive', '/v/@synologydrive', { held_by: [{ class: 'snapshot', bytes: 40 }, { class: 'unknown', bytes: 3 }] })],
    ))
    expect(rows[0].heldBySnapshots).toBe(40)
  })

  it('uses the complete service summary once when full rows are partitioned by folder', () => {
    const a = entry('hyper_backup_cache', '/v/a/x.hbk', {
      within: { path: '/v/a' }, held_by: [{ class: 'snapshot', bytes: 2 }],
    })
    const b = entry('hyper_backup_cache', '/v/b/y.hbk', {
      within: { path: '/v/b' }, held_by: [{ class: 'snapshot', bytes: 3 }],
    })
    const rows = packageRows(result([line('hyper_backup_cache', 10, { paths: 2 })], [a, b]))
    expect(rows).toHaveLength(1)
    expect(rows[0].physical).toBe(10)
    expect(rows[0].heldBySnapshots).toBe(5)
    expect(rows[0].paths.map(path => path.path)).toEqual(['/v/a/x.hbk', '/v/b/y.hbk'])
    // A ranking limit may omit a partition; it cannot prove complete holders.
    expect(packageRows(result([line('hyper_backup_cache', 10, { paths: 2 })], [a]))[0].heldBySnapshots).toBeNull()
  })

  it('leaves the snapshot-held bytes unknown when the answer lists no entry for the line', () => {
    expect(packageRows(result([line('drive', 100, { path: '/v/@synologydrive' })]))[0].heldBySnapshots).toBeNull()
  })

  it('reads zero snapshot-held bytes from an entry holding none', () => {
    const rows = packageRows(result([line('d', 5, { path: '/v/@d' })], [entry('d', '/v/@d')]))
    expect(rows[0].heldBySnapshots).toBe(0)
  })

  it('shows the paths a line adds up from its entry, at most the first 20 and the full count', () => {
    const paths = Array.from({ length: LISTED_PATHS }, (_, i) => ({ path: `/v/s${i}/x.hbk` }))
    const aggregated = entry('hb', '/v/s0/x.hbk', {
      service: { id: 'hb', names: '*.hbk', managed_by: 'hb', manage_in: 'x', manage_in_id: 'hyper_backup_cache', confirmed: true, paths, path_count: 45 },
    })
    const [row] = packageRows(result([line('hb', 9, { paths: 45 })], [aggregated]))
    expect(row.paths).toHaveLength(LISTED_PATHS)
    expect(row.pathCount).toBe(45)
  })

  it('falls back to the line path when no entry lists paths', () => {
    expect(packageRows(result([line('d', 5, { path: '/v/@d' })]))[0].paths).toEqual([{ path: '/v/@d' }])
  })

  it('gives each path of a service that lists each path its own row', () => {
    const rows = packageRows(result([
      line('docker', 9, { path: '/v/a/@docker' }),
      line('docker', 8, { path: '/v/b/@docker' }),
    ]))
    expect(new Set(rows.map(row => row.key)).size).toBe(2)
  })

  it('keeps the hex path of a path that is not text', () => {
    const [row] = packageRows(result([line('d', 5, { path: '/v/@d�', path_hex: 'aa' })]))
    expect(row.paths).toEqual([{ path: '/v/@d�', path_hex: 'aa' }])
  })
})

describe('findRow', () => {
  const rows = packageRows(result([
    line('docker', 9, { path: '/v/a/@docker' }),
    line('docker', 8, { path: '/v/b/@docker' }),
    line('hyper_backup_cache', 7, { paths: 4 }),
  ]))

  it('finds the row of one path of a service that lists each path', () => {
    expect(findRow(rows, 'docker', '/v/b/@docker')?.physical).toBe(8)
  })

  it('finds the summed row of a service by its code whatever path was clicked', () => {
    expect(findRow(rows, 'hyper_backup_cache', '/v/s/x.hbk')?.physical).toBe(7)
  })

  it('finds the first row of a service when the path matches none', () => {
    expect(findRow(rows, 'docker', '/v/zzz')?.physical).toBe(9)
  })

  it('finds nothing for an unknown service', () => {
    expect(findRow(rows, 'nope', '/v/x')).toBeUndefined()
  })
})

describe('the package area', () => {
  // Explore names these rows as data a package manages: the
  // Packages page must list them under the same package, wherever they lie.
  it('lists package data kept in shared folders too, as Explore names it', () => {
    const inShares = [
      'surveillance_recordings', 'hyper_backup', 'active_backup_data', 'active_backup_microsoft_365_data',
      'active_backup_google_workspace_data', 'active_backup_other', 'snapshot_view',
    ]
    const rows = packageRows(result([...inShares.map(service => line(service, 100)), line('container_manager', 5)]))
    expect(rows.map(row => row.service).sort()).toEqual([...inShares, 'container_manager'].sort())
  })

  it('lists an Active Backup destination folder with its size and what snapshots hold of it', () => {
    const path = '/volume1/ABB FS symbolic link test'
    const abb = entry('active_backup_data', path, {
      metrics: metrics(1_600_000_000_000),
      held_by: [{ class: 'snapshot', bytes: 494_500_000_000 }, { class: 'reflink', bytes: 8_700_000_000 }],
    })
    const rows = packageRows(result([
      line('active_backup_data', 1_600_000_000_000, { path, metrics: metrics(1_600_000_000_000, { reclaimable: 1_100_000_000_000 }) }),
      line('active_backup', 271_500_000, { path: '/volume1/@ActiveBackup' }),
    ], [abb]), ['/volume1/ABB FS symbolic link test', '/volume1/NetBackup'])
    expect(rows[0]).toMatchObject({
      service: 'active_backup_data', path, physical: 1_600_000_000_000, canFree: 1_100_000_000_000,
      heldBySnapshots: 494_500_000_000, sharedFolder: 'ABB FS symbolic link test',
    })
    expect(rows[1]).toMatchObject({ service: 'active_backup', sharedFolder: null })
  })

  it('names the shared folder of a summed line from its first path', () => {
    const hbk = entry('hyper_backup', '/volume1/NetBackup/SUP-1.hbk', {
      service: {
        id: 'hyper_backup', names: '*.hbk', managed_by: 'hb', manage_in: 'x', manage_in_id: 'hyper_backup', confirmed: true,
        paths: [{ path: '/volume1/NetBackup/SUP-1.hbk' }, { path: '/volume1/Other/x.hbk' }], path_count: 2,
      },
    })
    const [row] = packageRows(result([line('hyper_backup', 10, { paths: 2 })], [hbk]), ['/volume1/Net', '/volume1/NetBackup'])
    expect(row.sharedFolder).toBe('NetBackup')
    expect(packageRows(result([line('hyper_backup', 10, { paths: 2 })], [hbk]))[0].sharedFolder).toBeNull()
  })

  it('still leaves out Recycle Bins and folder metadata', () => {
    const rows = packageRows(result([line('recycle_bin', 9), line('folder_metadata', 9), line('docker', 1)]))
    expect(rows.map(row => row.service)).toEqual(['docker'])
  })
})

describe('handling', () => {
  it('says this app can clear its own cache', () => {
    expect(handling('syno_reclaim')).toBe('here')
  })

  it('sends package-managed data to the package', () => {
    for (const service of ['synology_drive', 'container_manager', 'active_backup', 'surveillance_data', 'iscsi_luns', 'share_snapshots'])
      expect(handling(service)).toBe('in_package')
  })

  it('marks data the system or a package needs to run as not deletable', () => {
    for (const service of ['packages', 'package_data', 'package_config', 'databases', 'dsm_temp', 'dsm_quota', 'other_dsm_service_data'])
      expect(handling(service)).toBe('keep')
  })

  it('sends an unknown service to its package', () => {
    expect(handling('something_new')).toBe('in_package')
  })
})

describe('howKey', () => {
  it('shares one explanation among services that are freed the same way', () => {
    expect(howKey('package_data')).toBe('package_files')
    expect(howKey('package_temp')).toBe('package_files')
    expect(howKey('active_backup_volume')).toBe('active_backup')
    expect(howKey('container_manager')).toBe('container_manager')
  })

  it('falls back to naming the place for a service without its own explanation', () => {
    expect(howKey('something_new')).toBe('default')
  })
})
