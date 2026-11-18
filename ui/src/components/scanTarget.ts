// How a scan's targets read in one line: "Whole volume1", "share01", "share01 and 2 more".
import { t } from '@/i18n'
import type { Path } from '@/types'

/** The most scan targets one WebAPI call takes (src/webapi.rs MAX_SCAN_TARGETS). */
export const SCAN_TARGET_LIMIT = 256

/** True when a selection is too long for one `scan.start`; the whole volume is then the way to go. */
export function exceedsTargetLimit(count: number) {
  return count > SCAN_TARGET_LIMIT
}

/** The last path segment of a target, what the pages show as its name. */
export function targetName(path: string) {
  return path.slice(path.lastIndexOf('/') + 1) || path
}

export function scopeLabel(targets: Path[], volume: boolean) {
  const first = targetName(targets[0]?.path ?? '')
  if (volume) return t('common.scope.volume', { name: first })
  const extra = targets.length - 1
  return extra > 0 ? t('common.scope.more', { name: first, count: extra }) : first
}
