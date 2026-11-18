// A shared folder's settings in DSM Control Panel, where its Recycle Bin is
// turned on. DSM opens Control Panel › Shared Folder with the share's row
// selected; no launch parameter opens its Edit dialog, so the user clicks Edit.
import { openControlPanel } from '@synopackageland/sdk'

/** Opens Control Panel › Shared Folder on `share`. False when DSM launched nothing. */
export async function openShareSettings(share: string): Promise<boolean> {
  try {
    return (await openControlPanel({ page: 'shared-folder', select: share })).status === 'requested'
  } catch {
    return false
  }
}
