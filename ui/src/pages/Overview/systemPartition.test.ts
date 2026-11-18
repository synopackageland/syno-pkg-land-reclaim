// The system partition's state rules.
import type { VolumesDocument } from '@/types'
import { NO_WARNING_ABOVE_BYTES, systemPartitionLevel } from './systemPartition'

type SystemPartition = NonNullable<VolumesDocument['system_partition']>

const partition = (total: number, available: number): SystemPartition => ({
  mount: '/',
  source: '/dev/md0',
  fstype: 'ext4',
  total_bytes: total,
  available_bytes: available,
  nearly_full: false,
  // Keep a pre-A1 payload in these fallback-rule tests.
} as SystemPartition)

// §2's two worked partition sizes and its four thresholds.
const EXT4_8_5_GB = 8_500_000_000
const SMALL_2_5_GB = 2_500_000_000
const UPDATE_ROOM_BYTES = 519_000_000 // T2: 389 MB patch + 130 MB
const ONE_GIB = 1_073_741_824 // T3
const SEVENTY_PERCENT_FREE_ON_8_5_GB = 2_550_000_000 // T1
const NINE_HUNDRED_MIB = 900 * 1024 * 1024 // DSM counts 900 MB in 1024s

describe('systemPartitionLevel', () => {
  it('is normal with room to spare', () => {
    expect(systemPartitionLevel(partition(EXT4_8_5_GB, 5_000_000_000))).toBe('ok')
  })

  it.each(['ok', 'getting_full', 'full'] as const)(
    'prefers the CLI %s level when it conflicts with the derived state',
    (level) => {
      // Derived: full, full and getting full, each unlike the CLI's level.
      const available = level === 'full' ? NINE_HUNDRED_MIB : UPDATE_ROOM_BYTES - 1
      const cliPartition: SystemPartition = {
        ...partition(EXT4_8_5_GB, available),
        level,
      }
      expect(systemPartitionLevel(cliPartition)).toBe(level)
    },
  )

  it('is normal above 900 MiB available, whatever the other rules say', () => {
    expect(NO_WARNING_ABOVE_BYTES).toBe(NINE_HUNDRED_MIB)
    // T1: 70 % used.
    expect(systemPartitionLevel(partition(EXT4_8_5_GB, SEVENTY_PERCENT_FREE_ON_8_5_GB))).toBe('ok')
    // T3: below 1 GiB.
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, NINE_HUNDRED_MIB + 1))).toBe('ok')
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, NINE_HUNDRED_MIB))).toBe('getting_full')
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, NINE_HUNDRED_MIB - 1))).toBe('getting_full')
    // T4: below 5 % of 20 GB.
    expect(systemPartitionLevel(partition(20_000_000_000, NINE_HUNDRED_MIB + 1))).toBe('ok')
    expect(systemPartitionLevel(partition(20_000_000_000, NINE_HUNDRED_MIB))).toBe('full')
  })

  it('is normal above 900 MiB available even when the CLI says otherwise', () => {
    expect(systemPartitionLevel({ ...partition(EXT4_8_5_GB, NINE_HUNDRED_MIB + 1), level: 'getting_full' })).toBe('ok')
    expect(systemPartitionLevel({ ...partition(20_000_000_000, NINE_HUNDRED_MIB + 1), level: 'full' })).toBe('ok')
  })

  it('is full just below the update room, and getting full at it (T2)', () => {
    expect(systemPartitionLevel(partition(EXT4_8_5_GB, UPDATE_ROOM_BYTES - 1))).toBe('full')
    expect(systemPartitionLevel(partition(EXT4_8_5_GB, UPDATE_ROOM_BYTES))).toBe('getting_full')
  })

  it('is normal on a small partition from 1 GiB free (T3)', () => {
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, ONE_GIB))).toBe('ok')
  })

  it('is full on a small partition below the update room (T2 before T3)', () => {
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, UPDATE_ROOM_BYTES - 1))).toBe('full')
    expect(systemPartitionLevel(partition(SMALL_2_5_GB, UPDATE_ROOM_BYTES))).toBe('getting_full')
  })

  it('is full below 5 % available when that is the higher floor (T4)', () => {
    // 5 % of 12 GB (600 MB) is higher than the 519 MB update room, so it decides.
    expect(systemPartitionLevel(partition(12_000_000_000, 599_999_999))).toBe('full')
    expect(systemPartitionLevel(partition(12_000_000_000, 600_000_000))).toBe('getting_full')
  })

  it('is null when the partition cannot be read', () => {
    expect(systemPartitionLevel(null)).toBeNull()
    expect(systemPartitionLevel(undefined)).toBeNull()
    expect(systemPartitionLevel(partition(0, 0))).toBeNull()
  })
})
