// The Duplicates page's calls (docs/duplicates.md, "WebAPI contract"). They go
// through api() like every other page: the package WebAPI in a build, the
// dev fixture backend (src/api/duplicatesFixture.ts) on the dev server and in
// tests.
import { api, type PathParam } from '@/api'
import type { Envelope } from '@/types'
import type { DuplicateCategory, DuplicateCopy, DuplicateGroup, DuplicateItem, DuplicateList, DuplicatePage } from '@/types/duplicates'

/** Groups one `duplicates.scope` or `duplicates.copies` page holds at most. */
export const PAGE_LIMIT = 200

/**
 * The volume has no duplicate data to answer from: not Btrfs, never scanned,
 * or scanned before the scan recorded candidates. A new scan fixes it.
 */
/**
 * Whether an unavailable answer is fixed by a new whole-volume scan: there is
 * no such report yet, or it predates duplicate candidates. Any other reason,
 * such as a refused read, is an error a scan does not fix.
 */
export function needsScan(detail?: string): boolean {
  return !!detail && /\bscan\b/i.test(detail)
}

export class DuplicatesUnavailable extends Error {
  constructor(readonly detail?: string) {
    super('duplicates_unavailable')
    this.name = 'DuplicatesUnavailable'
  }
}

/** A document that failed is an error here: its numbers are all empty. */
function answered<T extends Envelope>(document: T): T {
  if (document.completion !== 'failed') return document
  const error = document.errors?.[0]
  throw new DuplicatesUnavailable(error?.message)
}

export type ScopeQuery = {
  volume: string
  /** A folder on the volume; the volume itself when left out. */
  path?: string
  list?: DuplicateList
  category?: DuplicateCategory
  offset?: number
  limit?: number
}

export async function duplicatesScope({ volume, path, list, category, offset, limit }: ScopeQuery) {
  return answered(await api('duplicates.scope', {
    volume,
    ...(path ? { path } : {}),
    ...(list ? { list } : {}),
    ...(category ? { category } : {}),
    ...(offset ? { offset } : {}),
    ...(limit ? { limit } : {}),
  }))
}

/** Every group id of a list in the scope, page by page: what Select all takes. */
export async function allGroups(query: Omit<ScopeQuery, 'offset' | 'limit'>, most: number): Promise<DuplicateGroup[]> {
  const groups: DuplicateGroup[] = []
  for (let offset = 0; groups.length < most; offset += PAGE_LIMIT) {
    const answer = await duplicatesScope({ ...query, offset, limit: PAGE_LIMIT })
    groups.push(...answer.groups.items)
    if (!answer.groups.items.length || offset + PAGE_LIMIT >= answer.groups.total) break
  }
  return groups.slice(0, most)
}

/** Every copy of a group, with its role in the scope. */
export async function allCopies(params: { volume: string; group_id: string; scope?: string }) {
  const copies: DuplicateCopy[] = []
  let group: DuplicateGroup | null = null
  for (let offset = 0; ; offset += PAGE_LIMIT) {
    const answer = answered(await api('duplicates.copies', {
      volume: params.volume,
      group_id: params.group_id,
      ...(params.scope ? { scope: params.scope } : {}),
      ...(offset ? { offset } : {}),
      limit: PAGE_LIMIT,
    }))
    copies.push(...answer.copies.items)
    group ??= answer.group
    if (!answer.copies.items.length || copies.length >= answer.copies.total) break
  }
  return { group, copies }
}

/** Identical items one expanded partly-identical row loads at a time. */
export const ITEMS_LIMIT = 50

/**
 * One page of a `folder_partial` group's identical items. They come in the
 * `duplicates.copies` answer, paged by its offset and limit.
 */
export async function partialItems(params: { volume: string; group_id: string; scope?: string; offset?: number }): Promise<DuplicatePage<DuplicateItem>> {
  const offset = params.offset ?? 0
  const answer = answered(await api('duplicates.copies', {
    volume: params.volume,
    group_id: params.group_id,
    ...(params.scope ? { scope: params.scope } : {}),
    ...(offset ? { offset } : {}),
    limit: ITEMS_LIMIT,
  }))
  return answer.items ?? { total: 0, offset, items: [] }
}

/** The user keeps (or no longer keeps) one copy; stored on the NAS. Answers the group's row in the scope. */
export async function keepCopy(params: { volume: string; group_id: string; path: PathParam; keep: boolean; scope?: string }) {
  const { scope, ...rest } = params
  return answered(await api('duplicates.keep', { ...rest, ...(scope ? { scope } : {}) }))
}

/** The volume's preferred shares, in order. */
export async function preferredShares(volume: string) {
  return answered(await api('duplicates.preferred_shares', { volume }))
}

/**
 * Replaces the volume's preferred shares. The answers then say
 * `recalculating` until the analysis runs again, so it is started here.
 */
export async function setPreferredShares(volume: string, shares: string[]) {
  const answer = answered(await api('duplicates.preferred_shares', { volume, shares }))
  if (answer.recalculating) await analyze(volume)
  return answer
}

/** Starts the volume's duplicate analysis, or joins the one that runs. */
export async function analyze(volume: string) {
  return api('duplicates.analyze', { volume })
}

/** Whether a finished analysis job failed because the volume has no duplicate data, e.g. a report from before candidates were recorded. */
export async function analysisUnavailable(jobId: string): Promise<boolean> {
  const result = await api('job.result', { job_id: jobId }) as unknown as Envelope
  return result.completion === 'failed' && !!result.errors?.some(error => error.code === 'duplicates_unavailable' && needsScan(error.message))
}
