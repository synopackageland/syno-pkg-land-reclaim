// Each shared folder's Recycle Bin setting, read best-effort in the
// administrator's own DSM session (SYNO.Core.Share list, `recyclebin`), so the
// Duplicates page can say before Review which shares cannot be cleaned up:
// cleanup only moves items into a share's Recycle Bin. It is read fresh each
// time; null outside DSM or when DSM did not answer, and the page then says
// nothing (Review still checks each share with synoshare).
import type { Context } from '@synopackageland/sdk'
import { dsmWebApi } from './dsmWebApi'

export type RecycleBin = { enabled: boolean; adminOnly: boolean }
/** Shared folder name → its Recycle Bin. */
export type RecycleBins = Map<string, RecycleBin>
export type RecycleBinSource = (signal?: AbortSignal) => Promise<RecycleBins | null>

/** The `shares` of `SYNO.Core.Share list` with `additional: ["recyclebin"]`. */
export function parseRecycleBins(data: unknown): RecycleBins | null {
  const shares = (data as { shares?: unknown } | null)?.shares
  if (!Array.isArray(shares)) return null
  const bins: RecycleBins = new Map()
  for (const share of shares) {
    if (!share || typeof share !== 'object') continue
    const { name, enable_recycle_bin: enabled, recycle_bin_admin_only: adminOnly } = share as Record<string, unknown>
    if (typeof name === 'string' && typeof enabled === 'boolean') bins.set(name, { enabled, adminOnly: adminOnly === true })
  }
  return bins
}

export function createRecycleBinSource(ctx: Pick<Context, 'webApi'>): RecycleBinSource {
  return async signal => {
    try {
      return parseRecycleBins(await dsmWebApi(ctx, 'SYNO.Core.Share', 'list', 1, { additional: ['recyclebin'] }, signal))
    } catch {
      return null
    }
  }
}

const unavailable: RecycleBinSource = () => Promise.resolve(null)
let source: RecycleBinSource = unavailable

/** The DSM window's session; the dev page and tests answer from fixtures. */
export function setRecycleBinSource(next: RecycleBinSource | null) {
  source = next ?? unavailable
}

export function recycleBins(signal?: AbortSignal) {
  return source(signal)
}
