// The System partition view's rows, level and note.
import type { RootfsReportDocument } from '@/types'
import { deepCleanable, movableBytes, ownerRows, reportLevel, smallPartitionNote, stillShort, topHolder, topOwner } from './rootfsView'

const report = (overrides: Partial<RootfsReportDocument> = {}): RootfsReportDocument => ({
  schema_version: 1,
  command: 'rootfs',
  tool_version: 'test',
  generated_at: '2026-09-21T14:13:20Z',
  completion: 'complete',
  job_id: null,
  errors: [],
  warnings: [],
  filesystem: {
    mount: '/',
    source: '/dev/md0',
    fstype: 'ext4',
    total_bytes: 8_589_934_592,
    used_bytes: 7_659_934_592,
    free_bytes: 930_000_000,
    available_bytes: 930_000_000,
    used_percent: 89,
    inodes_total: 524_288,
    inodes_used: 133_742,
    nearly_full: false,
  },
  log_quota: null,
  walk: { entries: 133_740, allocated_bytes: 1, complete: true, stop_reason: null, unreadable_dirs: 0, elapsed_ms: 1 },
  usage: [],
  owners: [],
  owners_small: null,
  cleanable: { bytes: 0, required_destination_bytes: 0, items: 0, by_category: [], tools: [] },
  report_only: [],
  report_only_small: null,
  open_deleted: { bytes_estimate: 0, holders: [], unreadable_processes: 0 },
  not_walked: [],
  ...overrides,
})

type Owner = RootfsReportDocument['owners'][number]
const owner = (category: string, name: string | null, bytes: number, extra: Partial<Owner> = {}): Owner =>
  ({ category, name, bytes, percent_of_used: 0, ...extra }) as Owner

describe('ownerRows', () => {
  // The rows a test NAS showed (2026-10-31), before they were grouped.
  const listed = report({
    owners: [
      owner('user_tooling', '/usr/local', 1_111_588_864),
      owner('dsm_system', null, 929_275_904),
      owner('user_tooling', '/root', 483_172_352),
      owner('other', null, 259_092_480),
      owner('geoip_database', null, 85_913_600),
      owner('package_data', 'SMBService', 49_557_504, { display_name: 'SMB Service' }),
      owner('logs', null, 47_009_792),
      owner('log_center_db', null, 21_192_704),
      owner('dsm_keep', null, 11_120_640),
      owner('package_data', null, 5_128_192),
      owner('log_archive', null, 3_358_720),
      owner('update_leftover', null, 2_760_704),
      owner('package_data', 'DSMAgent', 4_415_488),
    ],
    owners_small: { below_bytes: 1_048_576, items: 43, bytes: 10_936_320 },
  })

  it('lists packages, what cleanup moves, DSM and the rest, largest first', () => {
    expect(ownerRows(listed).map(row => [row.kind, row.category, row.name, row.bytes])).toEqual([
      ['other', null, null, 1_111_588_864 + 483_172_352 + 259_092_480 + 10_936_320],
      ['dsm', null, null, 929_275_904 + 85_913_600 + 47_009_792 + 21_192_704 + 11_120_640 + 5_128_192],
      ['package', 'package_data', 'SMB Service', 49_557_504],
      ['package', 'package_data', 'DSMAgent', 4_415_488],
      ['cleanable', 'log_archive', null, 3_358_720],
      ['cleanable', 'update_leftover', null, 2_760_704],
    ])
  })

  it('folds what only DSM manages into one DSM row whose parts list each, read-only', () => {
    const dsm = ownerRows(listed).find(row => row.kind === 'dsm')!
    expect(dsm.parts.map(part => [part.category, part.bytes])).toEqual([
      ['dsm_system', 929_275_904],
      ['geoip_database', 85_913_600],
      ['logs', 47_009_792],
      ['log_center_db', 21_192_704],
      ['dsm_keep', 11_120_640],
      ['package_data', 5_128_192],
    ])
    expect(dsm.percent).toBeCloseTo((dsm.bytes / listed.filesystem.used_bytes) * 100, 6)
  })

  it('folds what the user installed, unknown files and small owners into Other', () => {
    const other = ownerRows(listed).find(row => row.kind === 'other')!
    expect(other.parts).toEqual([
      expect.objectContaining({ category: 'user_tooling', name: '/usr/local', bytes: 1_111_588_864 }),
      expect.objectContaining({ category: 'user_tooling', name: '/root', bytes: 483_172_352 }),
      expect.objectContaining({ category: 'other', name: null, bytes: 259_092_480 }),
      expect.objectContaining({ category: 'small', items: 43, below: 1_048_576, bytes: 10_936_320 }),
    ])
  })

  it('names a package by its display name, then its id, never an empty name', () => {
    const rows = ownerRows(report({
      owners: [
        owner('package_data', 'ActiveInsight', 3_000_000, { display_name: 'Active Insight' }),
        owner('package_data', 'Python2', 2_000_000, { display_name: '  ' }),
        owner('package_data', 'USBCopy', 1_500_000),
        owner('package_data', '', 1_200_000),
      ],
    }))
    expect(rows.filter(row => row.kind === 'package').map(row => row.name)).toEqual(['Active Insight', 'Python2', 'USBCopy'])
    expect(rows.find(row => row.kind === 'dsm')?.parts).toEqual([expect.objectContaining({ category: 'package_data', bytes: 1_200_000 })])
  })

  it('has no DSM or Other row when nothing falls in them', () => {
    expect(ownerRows(report())).toEqual([])
    const rows = ownerRows(report({ owners: [owner('update_leftover', null, 104_857_600)] }))
    expect(rows.map(row => row.kind)).toEqual(['cleanable'])
    expect(rows[0].parts).toEqual([])
  })
})

describe('reportLevel', () => {
  it('uses §2 on the report filesystem', () => {
    expect(reportLevel(report())).toBe('getting_full')
    expect(reportLevel(report({ filesystem: { ...report().filesystem, available_bytes: 380_000_000 } }))).toBe('full')
    expect(reportLevel(report({ filesystem: { ...report().filesystem, total_bytes: 8_589_934_592, available_bytes: 5_000_000_000 } }))).toBe('ok')
  })
})

describe('smallPartitionNote', () => {
  const small = (available: number, cleanable: number) =>
    report({
      filesystem: { ...report().filesystem, total_bytes: 2_500_000_000, available_bytes: available },
      cleanable: { bytes: cleanable, required_destination_bytes: cleanable, items: 1, by_category: [], tools: [] },
    })

  it('notes a small partition that stays getting full after cleaning', () => {
    expect(smallPartitionNote(small(850_000_000, 50_000_000))).toBe(true)
  })

  it('says nothing when cleaning would bring it back to normal', () => {
    expect(smallPartitionNote(small(1_000_000_000, 200_000_000))).toBe(false)
  })

  it('counts what a deep cleanup could move', () => {
    const doc = small(850_000_000, 50_000_000)
    doc.cleanable.deep = { ...doc.cleanable, bytes: 900_000_000, additional_bytes: 850_000_000 }
    expect(smallPartitionNote(doc)).toBe(false)
  })

  it('says nothing on a large partition', () => {
    expect(smallPartitionNote(report({ cleanable: { bytes: 0, required_destination_bytes: 0, items: 0, by_category: [], tools: [] } }))).toBe(false)
  })
})

describe('topHolder', () => {
  it('picks the process holding the most deleted bytes', () => {
    const holder = (comm: string, bytes: number) => ({ pid: 1, comm, inodes: 1, bytes_estimate: bytes, advice: { code: 'restart_holder', message: '' } as const })
    expect(topHolder([holder('a', 10), holder('b', 30), holder('c', 20)])?.comm).toBe('b')
    expect(topHolder([])).toBeNull()
  })
})

describe('topOwner', () => {
  it('picks the largest row, Other included', () => {
    const doc = report({
      owners: [
        { category: 'package_data', name: 'SynoFinder', bytes: 558_891_008, percent_of_used: 13.3 },
        { category: 'user_tooling', name: '/usr/local', bytes: 2_062_548_992, percent_of_used: 49.2 },
      ],
      owners_small: { below_bytes: 1_048_576, items: 2, bytes: 1_150_975 },
    })
    expect(topOwner(doc)?.kind).toBe('other')
  })
})

type Cleanable = RootfsReportDocument['cleanable']
const cleanable = (bytes: number, deep?: Partial<NonNullable<Cleanable['deep']>>): Cleanable => ({
  bytes,
  required_destination_bytes: bytes,
  items: bytes > 0 ? 1 : 0,
  by_category: [],
  tools: [],
  ...(deep ? { deep: { bytes: 0, required_destination_bytes: 0, items: 0, by_category: [], tools: [], additional_bytes: 0, ...deep } } : {}),
})

describe('deepCleanable and movableBytes', () => {
  it('reads the deep totals when the report has them', () => {
    const doc = report({ cleanable: cleanable(50, { bytes: 260, items: 9, additional_bytes: 210 }) })
    expect(deepCleanable(doc)?.bytes).toBe(260)
    expect(movableBytes(doc)).toBe(260)
  })

  it('falls back to the standard totals for a report from an older CLI', () => {
    const doc = report({ cleanable: cleanable(50) })
    expect(deepCleanable(doc)).toBeNull()
    expect(movableBytes(doc)).toBe(50)
  })

  it('has no deep totals when deep mode moves nothing', () => {
    expect(deepCleanable(report({ cleanable: cleanable(0, { bytes: 0, items: 0 }) }))).toBeNull()
  })
})

describe('stillShort', () => {
  const short = (available: number, extra: Partial<RootfsReportDocument> = {}) =>
    report({ filesystem: { ...report().filesystem, total_bytes: 2_442_780_672, available_bytes: available }, ...extra })

  it('says nothing when the cleanup leaves room for a DSM update', () => {
    expect(stillShort(short(300_000_000, { cleanable: cleanable(10, { bytes: 260_000_000, items: 9 }) }))).toBeNull()
  })

  it('names the room left and what the user installed when even deep is not enough', () => {
    const doc = short(216_223_744, {
      cleanable: cleanable(51_589_120, {
        bytes: 272_420_864,
        items: 4620,
        by_category: [{ category: 'tool_cache', bytes: 110_731_264, items: 4210 }],
      }),
      owners: [
        { category: 'user_tooling', name: '/usr/local', bytes: 291_504_128, percent_of_used: 13.1 },
        { category: 'user_tooling', name: '/root', bytes: 111_149_056, percent_of_used: 5 },
      ],
    })
    expect(stillShort(doc)).toEqual({ after: 488_644_608, userSoftware: 291_504_128, rootLeft: 417_792, heldOpen: 0 })
  })

  it('counts deleted files still held open, which a restart frees', () => {
    const doc = short(100_000_000, { open_deleted: { bytes_estimate: 12_000_000, holders: [], unreadable_processes: 0 } })
    expect(stillShort(doc)).toMatchObject({ after: 100_000_000, userSoftware: 0, rootLeft: 0, heldOpen: 12_000_000 })
  })
})
