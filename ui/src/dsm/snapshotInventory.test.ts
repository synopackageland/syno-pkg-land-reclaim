import { afterEach, describe, expect, it, vi } from 'vitest'
import { createDsmInventorySource, dsmInventorySource, parseVmmSnapshots, targetSnapshotStates } from './snapshotInventory'
import { fixtureInventory } from './snapshotInventoryFixture'
import targets from '../../fixtures/snapshots.list/volume1-targets.json'

const disk = '7d4e9b20-5a3c-4c6d-8e3f-1a2b3c4d5e6f'
const guest = '1a2b3c4d-7e8f-4a0b-9c1d-2e3f4a5b6c7d'
const share = targets.results[0]!.shares.find(s => s.share.path === disk)!
const time = Date.parse(share.snapshots[2]!.otime)
const snap = (locked: unknown = false) => ({ gsnap_id: `${guest}-gsnap-${time / 1000}`, gsnap_time: time / 1000,
  gsnap_name: 'GMT-2026.07.01-00.00.00', desc: 'Daily snapshot', is_user_locked: locked,
  is_main_exist: true, guest_disks: [{ blun_id: disk }] })

afterEach(() => { vi.unstubAllGlobals(); delete window.SYNO })

describe('VMM snapshot states', () => {
  it('reads both locked and unlocked scheduled snapshots by disk and exact creation time', () => {
    expect(parseVmmSnapshots({ gsnaps: [snap()] }).get(disk)?.get(time)).toEqual({ name: 'Daily snapshot', locked: false, immutable: null })
    expect(parseVmmSnapshots({ gsnaps: [snap(true)] }).get(disk)?.get(time)?.locked).toBe(true)
  })
  it('does not invent lock status for missing or malformed evidence', () => {
    expect(parseVmmSnapshots({ gsnaps: [snap('false')] }).get(disk)?.get(time)?.locked).toBeNull()
    expect(parseVmmSnapshots({ gsnaps: [{ ...snap(), gsnap_time: 'bad' }] }).size).toBe(0)
    expect(parseVmmSnapshots({ gsnaps: [{ ...snap(), is_main_exist: false }] }).size).toBe(0)
    expect(parseVmmSnapshots(null).size).toBe(0)
  })
  it('leaves colliding timestamps unknown rather than choosing one snapshot', () => {
    expect(parseVmmSnapshots({ gsnaps: [snap(false), { ...snap(true), gsnap_id: 'another' }] }).get(disk)?.has(time)).not.toBe(true)
  })
  it('adds VMM evidence when SAN does not list the snapshot, without changing other disks or dates', () => {
    const inventory = { ...fixtureInventory(), vmmSnapshots: parseVmmSnapshots({ gsnaps: [snap(true)] }) }
    const states = targetSnapshotStates(share, inventory)
    expect(states?.get(share.snapshots[2]!.name.path)?.locked).toBe(true)
    expect(states?.get(share.snapshots[0]!.name.path)?.locked).toBe(true)
    expect(states?.has(share.snapshots[4]!.name.path)).toBe(false)
    expect(targetSnapshotStates({ ...share, share: { path: 'another-disk' } }, inventory)?.has(share.snapshots[2]!.name.path)).toBe(false)
  })
  it('retains a storage-side lock when VMM reports an unlocked snapshot', () => {
    const inventory = fixtureInventory()
    inventory.vmmSnapshots = parseVmmSnapshots({ gsnaps: [{ ...snap(false), gsnap_time: Date.parse(share.snapshots[0]!.otime) / 1000 }] })
    expect(targetSnapshotStates(share, inventory)?.get(share.snapshots[0]!.name.path)?.locked).toBe(true)
  })
  it('reads VMM states through the window API without a global token', async () => {
    const webApi = vi.fn(async (api: string, method: string, version: number, params: Record<string, unknown>) => {
      if (api === 'SYNO.Core.ISCSI.LUN' && method === 'list') return { luns: [{ uuid: disk, name: `vdisk.${guest}.disk.2.0`, type_str: 'VDISK_BLUN' }] }
      if (api === 'SYNO.Virtualization.GuestProtect.Snap') {
        expect(version).toBe(1)
        expect(params).toEqual({ guest_id: guest, additional: ['guest_disks'], protect_id: '' })
        return { gsnaps: [snap()] }
      }
      return {}
    })
    const inventory = await createDsmInventorySource({ webApi }).inventory()
    expect(inventory?.vmmSnapshots.get(disk)?.get(time)?.locked).toBe(false)
    expect(webApi.mock.calls.filter(([api]) => api === 'SYNO.Virtualization.GuestProtect.Snap')).toHaveLength(1)
  })
  it('keeps LUN names and SAN states if VMM refuses the read', async () => {
    const webApi = vi.fn(async (api: string) => {
      if (api === 'SYNO.Core.ISCSI.LUN') return { luns: [{ uuid: disk, name: `vdisk.${guest}.disk.2.0`, type_str: 'VDISK_BLUN' }], snapshots: [] }
      throw { code: 'dsm_401' }
    })
    const inventory = await createDsmInventorySource({ webApi }).inventory()
    expect(inventory?.luns.has(disk)).toBe(true)
    expect(inventory?.vmmSnapshots.size).toBe(0)
  })
  it.each(['dsm_105', 'dsm_401', 'dsm_error'])('maps %s to missing inventory and unknown locks', async code => {
    const source = createDsmInventorySource({ webApi: async () => { throw { code } } })
    expect(await source.inventory()).toBeNull()
    expect(await source.shareSnapshots('share01')).toBeNull()
  })
  it('reads share locks from the window data', async () => {
    const webApi = vi.fn(async () => ({ snapshots: [{ time: 'snapshot-time', lock: true }] }))
    expect((await createDsmInventorySource({ webApi }).shareSnapshots('share01'))?.get('snapshot-time'))
      .toEqual({ locked: true, immutable: null })
    expect(webApi).toHaveBeenCalledWith('SYNO.Core.Share.Snapshot', 'list', 2, { name: 'share01', limit: -1, additional: ['lock', 'worm_lock'] })
  })
  it('makes no requests outside a DSM session', async () => {
    const fetcher = vi.fn(); vi.stubGlobal('fetch', fetcher)
    expect(await dsmInventorySource.inventory()).toBeNull()
    expect(await dsmInventorySource.shareSnapshots('share01')).toBeNull()
    expect(fetcher).not.toHaveBeenCalled()
  })
})
