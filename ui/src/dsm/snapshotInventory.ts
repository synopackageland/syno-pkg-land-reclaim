// What DSM itself calls each snapshot target and snapshot (names, lock and
// immutable status), read best-effort in the administrator's own DSM session
// through one DSM WebAPI adapter. The
// report only knows Btrfs subvolume names, which for a LUN or a VMM virtual
// disk are UUIDs. Reads only; any failure leaves the page on report names.

import { dsmWebApi } from './dsmWebApi'
import type { Context } from '@synopackageland/sdk'

/** One LUN, keyed by its UUID (the Btrfs subvolume name of its data). */
export type DsmLun = { name: string; type: string }
/** Lock state of one snapshot; null when DSM did not say. */
export type DsmSnapshotState = { name?: string; locked: boolean | null; immutable: boolean | null }

export type DsmInventory = {
  luns: Map<string, DsmLun>
  /** VMM guest id → guest name. */
  guests: Map<string, string>
  /** LUN snapshot UUID → its state. */
  lunSnapshots: Map<string, DsmSnapshotState>
  /** Local VMM disk UUID → exact snapshot creation time (milliseconds) → state. */
  vmmSnapshots: Map<string, Map<number, DsmSnapshotState>>
}

/** Shared folder → snapshot time name (`GMT+08-2026.10.01-00.00.01`) → its state. */
export type ShareSnapshotStates = Map<string, DsmSnapshotState>

export type DsmInventorySource = {
  inventory(signal?: AbortSignal): Promise<DsmInventory | null>
  shareSnapshots(share: string, signal?: AbortSignal): Promise<ShareSnapshotStates | null>
}

const text = (value: unknown) => (typeof value === 'string' ? value : '')
const flag = (value: unknown) => (typeof value === 'boolean' ? value : null)
const records = (value: unknown): Record<string, unknown>[] =>
  Array.isArray(value) ? value.filter((item): item is Record<string, unknown> => !!item && typeof item === 'object') : []

/** `SYNO.Core.ISCSI.LUN list`: every LUN category when `types` is omitted. */
export function parseLuns(data: unknown): Map<string, DsmLun> {
  const luns = new Map<string, DsmLun>()
  for (const lun of records((data as { luns?: unknown } | null)?.luns)) {
    const uuid = text(lun.uuid)
    if (uuid) luns.set(uuid, { name: text(lun.name), type: text(lun.type_str) })
  }
  return luns
}

/** `SYNO.Core.ISCSI.LUN list_snapshot` of every LUN, with the lock fields. */
export function parseLunSnapshots(data: unknown): Map<string, DsmSnapshotState> {
  const snapshots = new Map<string, DsmSnapshotState>()
  for (const snapshot of records((data as { snapshots?: unknown } | null)?.snapshots)) {
    const uuid = text(snapshot.uuid)
    if (!uuid) continue
    const user = flag(snapshot.is_user_locked)
    const action = flag(snapshot.is_action_locked)
    const apps = Array.isArray(snapshot.locked_app_keys) ? snapshot.locked_app_keys.length > 0 : null
    const known = [user, action, apps].filter((value): value is boolean => value !== null)
    snapshots.set(uuid, {
      name: text(snapshot.name) || undefined,
      locked: known.length ? known.some(Boolean) : null,
      immutable: flag(snapshot.is_worm_locked),
    })
  }
  return snapshots
}

/** `SYNO.Virtualization.Guest list`: guest id → name. */
export function parseGuests(data: unknown): Map<string, string> {
  const guests = new Map<string, string>()
  for (const guest of records((data as { guests?: unknown } | null)?.guests)) {
    const id = text(guest.guest_id)
    const name = text(guest.name)
    if (id && name) guests.set(id, name)
  }
  return guests
}

/** `SYNO.Core.Share.Snapshot list` v2: snapshot time name → lock state. */
export function parseShareSnapshots(data: unknown): ShareSnapshotStates {
  const states: ShareSnapshotStates = new Map()
  for (const snapshot of records((data as { snapshots?: unknown } | null)?.snapshots)) {
    const time = text(snapshot.time)
    if (time) states.set(time, { locked: flag(snapshot.lock), immutable: flag(snapshot.worm_lock) })
  }
  return states
}

/** VMM's local snapshots, including scheduled snapshots absent from SAN Manager. */
export function parseVmmSnapshots(data: unknown): DsmInventory['vmmSnapshots'] {
  const disks: DsmInventory['vmmSnapshots'] = new Map()
  const seen = new Set<string>()
  for (const snapshot of records((data as { gsnaps?: unknown } | null)?.gsnaps)) {
    const time = snapshot.gsnap_time
    if (typeof time !== 'number' || !Number.isFinite(time) || time < 0 || snapshot.is_main_exist !== true) continue
    for (const disk of records(snapshot.guest_disks)) {
      const id = text(disk.blun_id)
      if (!id) continue
      const key = `${id}:${time}`
      const states = disks.get(id) ?? new Map<number, DsmSnapshotState>()
      disks.set(id, states)
      // Two snapshots in the same second cannot safely identify one Btrfs snapshot.
      if (seen.has(key)) { states.delete(time * 1000); continue }
      seen.add(key)
      states.set(time * 1000, { name: text(snapshot.desc) || text(snapshot.gsnap_name) || undefined,
        locked: flag(snapshot.is_user_locked), immutable: null })
    }
  }
  return disks
}

/** Join within one disk and at the exact creation time; never guess from a label or a nearby time. */
export function targetSnapshotStates(target: { share: { path: string }; snapshots: readonly { name: { path: string }; otime: string | null }[] }, inventory: DsmInventory | null) {
  if (!inventory) return null
  const states = new Map(inventory.lunSnapshots)
  const vmm = inventory.vmmSnapshots.get(target.share.path)
  for (const snapshot of target.snapshots) {
    const state = snapshot.otime ? vmm?.get(Date.parse(snapshot.otime)) : undefined
    if (!state) continue
    const key = snapshot.name.path.slice(snapshot.name.path.lastIndexOf('/') + 1)
    const san = states.get(key)
    states.set(key, { ...state,
      locked: state.locked === true || san?.locked === true ? true : state.locked ?? san?.locked ?? null,
      immutable: san?.immutable ?? state.immutable })
  }
  return states
}

/** Whether a LUN is a Virtual Machine Manager disk or image. */
export function isVmmLun(lun: DsmLun) {
  return lun.type.startsWith('VDISK') || /^(vdisk|image)\./.test(lun.name)
}

async function settled<T>(promise: Promise<T>): Promise<T | null> {
  try { return await promise } catch { return null }
}

/** The DSM session's answers; null outside DSM or when every read failed. */
export function createDsmInventorySource(ctx: Pick<Context, 'webApi'>): DsmInventorySource {
  return {
    async inventory(signal) {
      const [luns, lunSnapshots] = await Promise.all([
        settled(dsmWebApi(ctx, 'SYNO.Core.ISCSI.LUN', 'list', 1, {}, signal).then(parseLuns)),
        settled(dsmWebApi(ctx, 'SYNO.Core.ISCSI.LUN', 'list_snapshot', 1, { additional: ['is_worm_locked', 'is_action_locked', 'locked_app_keys'] }, signal).then(parseLunSnapshots)),
      ])
      // VMM may not be installed; only its disks need these read-only calls.
      const ids = new Set([...(luns ?? new Map()).values()].filter(isVmmLun)
        .map(lun => /^vdisk\.([0-9a-f-]{36})\./i.exec(lun.name)?.[1]).filter((id): id is string => !!id))
      const [guests, snapshots] = await Promise.all([
        luns && [...luns.values()].some(isVmmLun)
          ? settled(dsmWebApi(ctx, 'SYNO.Virtualization.Guest', 'list', 2, {}, signal).then(parseGuests))
          : Promise.resolve(null),
        Promise.all([...ids].map(guest_id => settled(dsmWebApi(ctx, 'SYNO.Virtualization.GuestProtect.Snap', 'list', 1,
          { guest_id, protect_id: '', additional: ['guest_disks'] }, signal).then(parseVmmSnapshots)))),
      ])
      if (!luns && !lunSnapshots) return null
      const vmmSnapshots: DsmInventory['vmmSnapshots'] = new Map()
      for (const disks of snapshots) if (disks) for (const [id, states] of disks) vmmSnapshots.set(id, states)
      return { luns: luns ?? new Map(), guests: guests ?? new Map(), lunSnapshots: lunSnapshots ?? new Map(), vmmSnapshots }
    },
    async shareSnapshots(share, signal) {
      return settled(dsmWebApi(ctx, 'SYNO.Core.Share.Snapshot', 'list', 2, { name: share, limit: -1, additional: ['lock', 'worm_lock'] }, signal).then(parseShareSnapshots))
    },
  }
}

export const dsmInventorySource = createDsmInventorySource({})

let source: DsmInventorySource = dsmInventorySource

/** The dev page and tests answer from fixtures instead of a DSM session. */
export function setDsmInventorySource(next: DsmInventorySource) {
  source = next
}

export function inventorySource() {
  return source
}
