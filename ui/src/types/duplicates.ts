// The duplicates WebAPI contract (docs/duplicates.md, "WebAPI contract").
// No JSON schema exists for these documents yet: these types follow the
// answer structs of src/duplicates/command.rs field for field.
import type { Envelope, Path } from './index'

/** `folder_partial`: partly identical folders, a pair whose identical items are at least 80% of the smaller side's bytes. */
export type DuplicateKind = 'file' | 'folder' | 'folder_partial'
/** Identical folders, whole or partly: listed under `list=folders`. */
export const isFolderKind = (kind: DuplicateKind) => kind !== 'file'
/** A file group's category, from its extension. Folder groups have none. */
export type DuplicateCategory = 'photos' | 'videos' | 'music' | 'other'
/** Why the kept copy is the one kept, in the order the rule applies them. */
export type KeeperReason = 'user' | 'preferred_share' | 'unmovable' | 'in_kept_folder' | 'oldest' | 'shortest'
/**
 * `in_folder`: a file copy inside a copy of a listed identical folder; that folder group removes and counts it.
 * `unmovable`: a folder or file right below the volume (a shared folder itself, or outside every one): no cleanup moves it.
 */
export type DuplicateRole = 'keep' | 'keep_user' | 'remove' | 'unmovable' | 'in_folder'
/** `both`: large files in `groups`, identical folders in `folders`, in one answer. */
export type DuplicateList = 'files' | 'folders' | 'both'

/** What removing a kind's duplicates in a scope does. */
export type DuplicateAmount = { copies: number; can_free: number; frees_later: number }
/** `all` is `files` plus `folders`; photos, videos and music are parts of `files`. */
export type DuplicateTally = Record<'all' | 'files' | 'photos' | 'videos' | 'music' | 'folders', DuplicateAmount>

export type DuplicateGroupInfo = {
  /** 32 hex digits; a folder group's starts with `f`. Stable while the content is. */
  id: string
  kind: DuplicateKind
  /** The file or folder name. */
  name: Path
  /** Bytes of one copy; for `folder_partial`, of one side's identical items. */
  size: number
  /** A folder group's file count; for `folder_partial`, its identical items' files. */
  files?: number
  /** Every copy, those the user cannot open included. */
  copies_total: number
  category?: DuplicateCategory
  /** `folder_partial`: the identical items the row removes from a side. */
  same_items?: number
}

/**
 * A `folder_partial` row's side: `path` is the first side the row removes
 * from (in the scope first), `counterpart` the kept side. Deleting removes only the identical items: the side
 * folder, the items that differ and `.git` stay.
 */
export type DuplicatePartial = {
  path: Path
  same_items: number
  total_items: number
  same_files: number
  total_files: number
  same_bytes: number
  total_bytes: number
  counterpart: Path
}

/** One identical item of partly identical folders, relative to each side, without a leading `/`. */
export type DuplicateItem = { path: Path; kind: 'folder' | 'file'; files: number; bytes: number }

/** The kept copy. */
export type DuplicateKeeper = {
  path: Path
  share: string
  /** RFC 3339. */
  mtime: string
  reason: KeeperReason
  in_scope: boolean
}

export type DuplicateScopeCopy = {
  path: Path
  share: string
  /** RFC 3339. */
  mtime: string
  role: 'remove' | 'keep_user' | 'unmovable'
}

/** One row of a list (GroupView). */
export type DuplicateGroup = {
  group: DuplicateGroupInfo
  keeper: DuplicateKeeper
  /** The scope's copies other than the keeper, at most five. */
  in_scope: DuplicateScopeCopy[]
  /** All of the scope's copies other than the keeper. */
  in_scope_total: number
  /** Copies this cleanup removes: 0 when the user kept every copy here; the row stays so the choice can be undone. */
  removes: number
  /** Copies outside the scope, keeper and copies in identical folders excluded, by share. */
  elsewhere_by_share: { share: string; count: number }[]
  /** The shares of every copy the row removes, on the whole volume. */
  remove_shares: string[]
  /** Copies right below the volume other than the kept one: a shared folder itself, or outside every one. They stay. */
  unmovable: Path[]
  /** Copies inside copies of a listed identical folder: that folder group removes and counts them, not this row. */
  in_identical_folders: number
  can_free: number
  /** Bytes snapshots still hold: freed when those snapshots rotate. */
  frees_later: number
  /** A removed copy lies in `homes/<user>/Drive`. */
  touches_drive: boolean
  /** Every copy is a clone of one extent set: removing frees nothing. */
  shared: boolean
  /** `folder_partial` only. */
  partial?: DuplicatePartial
}

export type DuplicateAnalysis = {
  state: 'none' | 'stale' | 'ready'
  running: boolean
  job_id: string | null
  analyzed_at: string | null
  report_job_id: string | null
  complete: boolean
}

export type DuplicateUnchecked = {
  files: number
  bytes: number
  no_checksums: number
  compressed: number
  folders: number
  folder_bytes: number
}

export type DuplicatePage<T> = { total: number; offset: number; items: T[] }

export type DuplicateScopeDocument = Envelope & {
  command: 'duplicates-scope'
  volume: Path
  scope: Path
  /** null when the volume could not be read at all. */
  analysis: DuplicateAnalysis | null
  /** Preferred shares changed or an analysis runs: the numbers are the previous ones. */
  recalculating: boolean
  thresholds: { record_bytes: number; list_bytes: number }
  preferred_shares: string[]
  totals: DuplicateTally | null
  /** Child folders holding copies, by `all.can_free`; at the volume, its shares. */
  children: { path: Path; name: Path; tally: DuplicateTally }[]
  unchecked: DuplicateUnchecked | null
  list: DuplicateList
  category: DuplicateCategory | null
  touches_drive: boolean
  groups: DuplicatePage<DuplicateGroup>
  /** With `list` `both`: the identical folders, paged by the same offset and limit. */
  folders?: DuplicatePage<DuplicateGroup>
}

export type DuplicateCopy = {
  path: Path
  share: string
  mtime: string
  role: DuplicateRole
  in_scope: boolean
}

export type DuplicateCopiesDocument = Envelope & {
  command: 'duplicates-copies'
  volume: Path
  scope: Path
  group: DuplicateGroup | null
  copies: DuplicatePage<DuplicateCopy>
  /** `folder_partial`: the identical items, largest first, paged by the same offset and limit. */
  items?: DuplicatePage<DuplicateItem>
}

export type DuplicateKeepDocument = Envelope & {
  command: 'duplicates-keep'
  volume: Path
  scope: Path
  group: DuplicateGroup | null
  totals: DuplicateTally | null
}

export type DuplicatePreferredDocument = Envelope & {
  command: 'duplicates-preferred-shares'
  volume: Path
  shares: string[]
  recalculating: boolean
}

/** What `cleanup.plan` adds for a duplicates selection. */
/** A removed copy in no DSM shared folder is in a plan share marked `delete`: removed for good once confirmed. */
export type DuplicatePlanSummary = { copies: number; groups: number; kept: number; scan_finished?: number }

/** `cleanup.plan`'s duplicates selection: groups of one volume, each seen on the whole volume. */
export type DuplicatePlanSelection = { volume: string | Path; groups: string[] }
