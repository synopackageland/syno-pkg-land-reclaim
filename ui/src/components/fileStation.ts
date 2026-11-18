import { openFileInFileStation, openFolderInFileStation } from '@synopackageland/sdk'
import { volumeList } from '@/stores/volumes'
import type { Volume } from '@/types'

export type FileStationTarget = {
  path: string
  /** Set when the name is not UTF-8: `path` is then only a display form File Station cannot open. */
  path_hex?: string
  /** A folder opens as itself; anything else opens its folder with it selected. */
  kind: 'dir' | 'file' | 'service' | (string & {})
}

/** What File Station needs to know of a volume: its mount and DSM's shared folders on it. */
export type ShareVolume = Pick<Volume, 'mount' | 'shares' | 'notes'>

/** Why File Station cannot show a target; null when it can. */
export type FileStationBlock = 'not_shared' | 'unreadable_name' | 'shares_unknown'

/** volumes.list notes that leave a volume's share list empty because it could not be read. */
const SHARES_UNREAD = ['shares_unavailable', 'subvolumes_unavailable', 'subvolume_id_failed', 'metadata_unavailable', 'open_failed']

const under = (path: string, root: string) => path === root || path.startsWith(`${root}/`)

/**
 * Where a target sits for File Station: the volume it is on and the shared
 * folder that holds it, matched exactly against the share list volumes.list
 * read from DSM. A name is never guessed from the path: a top-level folder
 * that is not a share, DSM's own @ folders and a name that differs only in
 * case are not shares, so File Station is never sent to a wrong folder.
 */
function locate(path: string, volumes: readonly ShareVolume[]): { volume: ShareVolume } | FileStationBlock {
  const volume = volumes.find(candidate => under(path, candidate.mount.path.replace(/\/+$/, '')))
  if (!volume) return 'shares_unknown'
  const shares = volume.shares.filter(share => !share.path.path_hex)
  if (shares.some(share => under(path, share.path.path))) return { volume }
  if (!shares.length && volume.notes.some(note => SHARES_UNREAD.includes(note.code))) return 'shares_unknown'
  return 'not_shared'
}

/** Whether File Station can show a target, against the volumes' shared folders (default: the loaded volumes.list). */
export function fileStationBlock(target: FileStationTarget, volumes: readonly ShareVolume[] = volumeList.value): FileStationBlock | null {
  if (target.path_hex) return 'unreadable_name'
  const place = locate(target.path, volumes)
  if (typeof place === 'string') return place
  // The SDK's own path rule (no '.', '..' or control characters), asked without launching anything.
  const sdkAccepts = openFolderInFileStation(target.path, { mount: place.volume.mount.path, desktop: { AppLaunch: () => {} } })
  return sdkAccepts ? null : 'not_shared'
}

/** Opens a folder as itself and a file selected in its folder. False when nothing was launched. */
export function openInFileStation(target: FileStationTarget, volumes: readonly ShareVolume[] = volumeList.value): boolean {
  if (fileStationBlock(target, volumes)) return false
  const place = locate(target.path, volumes)
  if (typeof place === 'string') return false
  const options = { mount: place.volume.mount.path }
  return target.kind === 'file'
    ? openFileInFileStation(target.path, options)
    : openFolderInFileStation(target.path, options)
}

/** The i18n key for why File Station cannot show a target. */
export function fileStationBlockKey(block: FileStationBlock): string {
  return {
    not_shared: 'common.fileStation.notShared',
    unreadable_name: 'common.fileStation.unreadableName',
    shares_unknown: 'common.fileStation.sharesUnknown',
  }[block]
}
