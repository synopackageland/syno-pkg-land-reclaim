// What each snapshot target is called and how much its snapshots hold, for
// the picker. The report names a target by its Btrfs subvolume: a shared
// folder by its name, a LUN or a VMM virtual disk by a UUID, which DSM's own
// inventory turns into the name people know.
import { isVmmLun, type DsmInventory } from '@/dsm/snapshotInventory'
import type { SnapshotsDocument } from '@/types'

type Share = SnapshotsDocument['results'][number]['shares'][number]

export type TargetKind = 'share' | 'lun' | 'vmm' | 'other'
export const TARGET_KINDS: readonly TargetKind[] = ['share', 'lun', 'vmm', 'other']

export type TargetName = {
  kind: TargetKind
  /** Shown first; never a bare UUID. */
  name: string
  /** A short id that tells same-named targets apart, when there is one. */
  id?: string
}

/** DSM management destination comes from the typed inventory category, never display text. */
export function snapshotManager(kind: TargetKind) {
  if (kind === 'vmm') return { appId: 'SYNO.SDS.Virtualization.Application', placeId: 'virtual_machine_manager' }
  if (kind === 'lun') return { appId: 'SYNO.SDS.iSCSI.Application', placeId: 'san_manager' }
  return { appId: 'SYNO.SDS.DisasterRecovery.Application', placeId: 'snapshot_replication' }
}

/** How sure the space figure is. */
export type SpaceCertainty = 'exact' | 'upTo' | 'range' | 'unknown'
export type TargetSpace = { certainty: SpaceCertainty; bytes: number; upper: number }

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const TIERING = /^_TIERFS_/
/** `vdisk.<guest>.<disk>.<n>.<n>` and `image.<image>.<disk>.<n>.<n>`. */
const VMM_LUN = /^(vdisk|image)\.([0-9a-f-]{36})\./i

const short = (uuid: string) => uuid.slice(0, 8)

export type NameLabels = {
  unnamedLun: (id: string) => string
  vmmImage: string
  vmmDisk: string
  tiering: string
  unnamed: (id: string) => string
}

/** A target's display name and kind, from the report and DSM's inventory. */
export function describeTarget(share: Share, inventory: DsmInventory | null, labels: NameLabels): TargetName {
  const path = share.share.path
  const lun = inventory?.luns.get(path)
  if (lun) {
    if (!isVmmLun(lun)) return { kind: 'lun', name: lun.name || labels.unnamedLun(short(path)) }
    const match = VMM_LUN.exec(lun.name)
    if (match?.[1]?.toLowerCase() === 'image') return { kind: 'vmm', name: labels.vmmImage, id: short(path) }
    const guest = match ? inventory?.guests.get(match[2]!) : undefined
    return { kind: 'vmm', name: guest ?? labels.vmmDisk, id: short(path) }
  }
  if (share.manage_in_id === 'san_manager') return { kind: 'lun', name: labels.unnamedLun(short(path)) }
  if (TIERING.test(path)) return { kind: 'other', name: labels.tiering, id: short(path.replace(TIERING, '')) }
  if (UUID.test(path)) return { kind: 'other', name: labels.unnamed(short(path)) }
  return { kind: 'share', name: path }
}

/** What a target's snapshots hold, and how sure that is. */
export function targetSpace(share: Share): TargetSpace {
  if (!share.snapshots.some(snapshot => snapshot.materialized)) return { certainty: 'unknown', bytes: 0, upper: 0 }
  const held = share.held_only_by_snapshots_bytes
  const unknown = share.unknown_bytes ?? 0
  if (!unknown) return { certainty: 'exact', bytes: held, upper: held }
  if (!held) return { certainty: 'upTo', bytes: unknown, upper: unknown }
  return { certainty: 'range', bytes: held, upper: held + unknown }
}

export type TargetOption = { value: string; label: string; kind: TargetKind; space: TargetSpace; name: TargetName }
export type TargetGroup = { kind: TargetKind; options: TargetOption[] }

/** Largest first; a target whose space is unknown goes last. */
function bySpace(a: TargetOption, b: TargetOption) {
  const rank = (option: TargetOption) => (option.space.certainty === 'unknown' ? -1 : option.space.upper)
  return rank(b) - rank(a) || a.name.name.localeCompare(b.name.name)
}

/**
 * Options grouped by kind; each group largest first, and the group holding
 * the most first. Same-named targets carry their short id.
 */
export function targetGroups(shares: Share[], inventory: DsmInventory | null, labels: NameLabels & {
  space: (space: TargetSpace) => string
}): TargetGroup[] {
  const named = shares.map(share => ({ share, name: describeTarget(share, inventory, labels) }))
  const counts = new Map<string, number>()
  for (const { name } of named) counts.set(`${name.kind}:${name.name}`, (counts.get(`${name.kind}:${name.name}`) ?? 0) + 1)
  const options: TargetOption[] = named.map(({ share, name }) => {
    const space = targetSpace(share)
    const title = name.id && (counts.get(`${name.kind}:${name.name}`) ?? 0) > 1 ? `${name.name} (${name.id})` : name.name
    return { value: share.share.path, label: `${title} · ${labels.space(space)}`, kind: name.kind, space, name }
  })
  const groups = TARGET_KINDS.map(kind => ({ kind, options: options.filter(option => option.kind === kind).sort(bySpace) }))
    .filter(group => group.options.length)
  const top = (group: TargetGroup) => (group.options[0] ? Math.max(group.options[0].space.upper, 0) : 0)
  return groups.sort((a, b) => top(b) - top(a) || TARGET_KINDS.indexOf(a.kind) - TARGET_KINDS.indexOf(b.kind))
}
