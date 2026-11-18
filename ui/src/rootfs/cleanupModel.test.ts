import { describe, expect, it } from 'vitest'
import {
  scanningVolumeMounts,
  defaultDestination,
  refusalMessageKey,
  shareReasons,
  splitSmallMoveItems,
  type CleanupShare,
} from './cleanupModel'
import type { RootfsMoveItem as CleanupMoveItem } from './cleanupTypes'

const share = (name: string, mount: string, freeBytes: number): CleanupShare => ({
  name,
  path: `${mount}/${name}`,
  mount,
  freeBytes,
})

describe('rootfs cleanup destination model', () => {
  it('preselects the eligible share with the most free space', () => {
    const shares = [share('backup', '/volume1', 900), share('small', '/volume2', 200), share('archive', '/volume3', 1200)]
    const result = defaultDestination(shares, 300)

    expect(result?.path).toBe('/volume3/archive')
  })

  it('does not disqualify a destination just because its volume is active', () => {
    const shares = [share('small', '/volume1', 200), share('busy', '/volume2', 1000), share('ready', '/volume3', 500)]
    const result = defaultDestination(shares, 300)

    expect(result?.path).toBe('/volume2/busy')
  })

  it('keeps the free-space gate and identifies only scans for the informational note', () => {
    const candidate = share('backup', '/volume1', 100)

    expect(shareReasons(candidate, 300)).toEqual(['space'])
    expect(scanningVolumeMounts([{ kind: 'scan', state: 'running', filesystems: [{ mount: { path: '/volume1' } }] }])).toEqual(new Set(['/volume1']))
    expect(defaultDestination([candidate], 300)).toBeNull()
  })
})

describe('small move details', () => {
  it('folds files smaller than 1 MiB into one aggregate row', () => {
    const items: CleanupMoveItem[] = [
      { path: '/upd@te/tiny-a', bytes: 512 * 1024, kind: 'file', category: 'update_leftover' },
      { path: '/upd@te/tiny-b', bytes: 512 * 1024 - 1, kind: 'file', category: 'update_leftover' },
      { path: '/upd@te/exactly-one-mib', bytes: 1024 * 1024, kind: 'file', category: 'update_leftover' },
    ]

    expect(splitSmallMoveItems(items)).toEqual({
      visible: [items[2]],
      foldedCount: 2,
      foldedBytes: 1024 * 1024 - 1,
    })
  })
})

describe('rootfs plan refusal copy', () => {
  it.each([
    ['destination_full', 'rootfsClean.error.destinationFull'],
    ['busy', 'rootfsClean.error.busy'],
    ['apply_running', 'rootfsClean.error.rootfsBusy'],
    ['backend_busy', 'rootfsClean.error.rootfsBusy'],
    ['rootfs_busy', 'rootfsClean.error.rootfsBusy'],
    ['destination_changed', 'rootfsClean.error.stale'],
    ['apply_running', 'rootfsClean.error.rootfsBusy'],
    ['walk_stopped', 'rootfsClean.error.incomplete'],
    ['processes_unreadable', 'rootfsClean.error.incomplete'],
    ['dsm_update_active', 'rootfsClean.error.dsmUpdate'],
    ['plan_stale', 'rootfsClean.error.stale'],
    ['root_changed', 'rootfsClean.error.stale'],
    ['destination_changed', 'rootfsClean.error.stale'],
    ['plan_untrusted', 'rootfsClean.error.untrusted'],
    ['plan_dir_untrusted', 'rootfsClean.error.untrusted'],
    ['not_root', 'rootfsClean.error.notRoot'],
    ['unexpected_code', 'rootfsClean.error.unknown'],
  ])('maps %s to localized copy', (code, key) => {
    expect(refusalMessageKey(code)).toBe(key)
  })

  it('separates a rootfs writer lock from a busy volume', () => {
    expect(refusalMessageKey('busy', 'rootfs-apply')).toBe('rootfsClean.error.rootfsBusy')
    expect(refusalMessageKey('busy', 'scan')).toBe('rootfsClean.error.busy')
  })
})
