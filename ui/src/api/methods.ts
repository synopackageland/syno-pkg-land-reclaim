// Every WebAPI method with its params
// and the CLI document it resolves to.
import type { Target as CleanupPlanTarget } from '@/types/schema/cleanup-plan-params'
import type {
  CacheDocument,
  CancelDocument,
  ModeDocument,
  CleanupApplyDocument,
  DetachDocument,
  HistoryDocument,
  InspectDocument,
  JobsDocument,
  OverviewDocument,
  PauseDocument,
  Path,
  RecycleDocument,
  RootfsApplyDocument,
  RootfsDocument,
  RootfsHistoryDocument,
  RootfsPlanResultDocument,
  CleanupPlanDocument,
  ScanDocument,
  SessionDocument,
  SimulateDocument,
  SnapshotsDocument,
  TopDocument,
  VolumesDocument,
  WatchFreeDocument,
  WhyNotFreedDocument,
} from '@/types'
import type {
  DuplicateCategory,
  DuplicateCopiesDocument,
  DuplicateKeepDocument,
  DuplicateList,
  DuplicatePlanSelection,
  DuplicatePreferredDocument,
  DuplicateScopeDocument,
} from '@/types/duplicates'

/** A path as the CLI printed it, or a plain string. */
export type PathParam = string | Path
export type ScanMode = 'fast' | 'gentle'
type NoParams = Record<string, never>
type VolumeParam = { volume: PathParam }
type JobParam = { job_id: string }
/** No schema yet (doctor, cleanup plan, support bundle: GAP-5 and later). */
type Untyped = Record<string, unknown>
/** What cleanup.select staged so far. */
export type CleanupSelectDocument = { command: 'cleanup-select'; completion: string; selection: string; paths: number; groups: number; errors?: { code: string; message?: string }[] }

/** `config set --json` (schemas/v1/config-set.schema.json); a refusal prints no `config`. */
type ConfigSetResult = {
  enables_cleanup?: boolean
  config?: { cleanup_enabled: boolean; mutations_disabled: boolean }
}

/** The root helper's Application Portal status; `error` when DSM refused a change. */
export type PortalDocument = {
  state: 'active' | 'inactive' | 'unavailable'
  alias?: string
  entry?: 'registered' | 'unregistered' | 'cleanup-required'
  error?: 'PORTAL_FAILED'
  exit?: number
}

export interface Methods {
  'session.get': { params: NoParams; result: SessionDocument & { user?: string; is_admin?: boolean } }
  'setup.cleanup': { params: { mutations_disabled: boolean }; result: ConfigSetResult }
  'volumes.list': { params: NoParams; result: VolumesDocument }
  'doctor.run': { params: VolumeParam; result: Untyped }
  'scan.start': { params: { targets: PathParam[]; mode: ScanMode; snapshots?: boolean | 'all'; snapshot_report?: string; attribution_report?: string }; result: DetachDocument }
  'jobs.list': { params: NoParams; result: JobsDocument }
  'job.pause': { params: JobParam; result: PauseDocument }
  'job.resume': { params: JobParam & { mode?: ScanMode; fresh?: boolean }; result: DetachDocument }
  'job.cancel': { params: JobParam; result: CancelDocument }
  'job.mode': { params: JobParam & { mode: ScanMode }; result: ModeDocument }
  'job.result': { params: JobParam; result: ScanDocument | CleanupApplyDocument | RootfsApplyDocument | CleanupPlanDocument | RootfsPlanResultDocument | RootfsDocument }
  'history.list': { params: NoParams; result: HistoryDocument }
  'overview.get': { params: VolumeParam; result: OverviewDocument }
  'top.list': {
    params: {
      target: PathParam
      kind: 'all' | 'dirs' | 'files'
      by?: 'reclaimable' | 'logical' | 'physical' | 'shared' | 'held'
      limit?: number
      /** Answer from this saved report (scan job ID) instead of the newest covering one. */
      report?: string
    }
    result: TopDocument
  }
  'inspect.get': { params: { paths: PathParam[] }; result: InspectDocument }
  'simulate.get': { params: { paths: PathParam[] }; result: SimulateDocument }
  'snapshots.list': { params: { target: PathParam; simulate?: string }; result: SnapshotsDocument }
  'why_not_freed.get': { params: VolumeParam & { processes?: boolean }; result: WhyNotFreedDocument }
  'recycle.list': { params: { target: PathParam; oldest?: number; report?: string }; result: RecycleDocument }
  'watch_free.sample': { params: VolumeParam; result: WatchFreeDocument }
  /** One chunk of a selection staged for cleanup.plan, so a selection of any size never goes in one request. */
  'cleanup.select': { params: { selection?: string; paths?: (string | CleanupPlanTarget)[]; groups?: string[] }; result: CleanupSelectDocument }
  /**
   * `paths` may be empty when `duplicates` or `selection` is given; one duplicates scope per plan.
   * `selection` is what cleanup.select staged, its paths and groups; the plan uses it up.
   */
  'cleanup.plan': { params: { paths?: (string | CleanupPlanTarget)[]; duplicates?: DuplicatePlanSelection; selection?: string }; result: DetachDocument }
  'cleanup.apply': { params: { plan_id: string; confirm_move: true; confirm_delete?: true }; result: DetachDocument | CleanupApplyDocument }
  'cache.status': { params: VolumeParam; result: CacheDocument }
  'cache.clear': { params: VolumeParam; result: CacheDocument }
  'support_bundle.get': { params: VolumeParam; result: Untyped }
  /** A saved report within max_age_s answers at once; otherwise a job walks `/`. */
  'rootfs.report': { params: { max_age_s?: number }; result: DetachDocument | RootfsDocument }
  'rootfs.plan': { params: { to: PathParam; mode?: 'standard' | 'deep' }; result: DetachDocument }
  'rootfs.apply': { params: { plan_id: string; confirm_no_restore: true }; result: DetachDocument | RootfsApplyDocument }
  'rootfs.history': { params: NoParams; result: RootfsHistoryDocument }
  /** A planning job; a second request for the volume joins the running one. */
  'duplicates.analyze': { params: VolumeParam; result: DetachDocument }
  'duplicates.scope': {
    params: VolumeParam & { path?: PathParam; list?: DuplicateList; category?: DuplicateCategory; offset?: number; limit?: number }
    result: DuplicateScopeDocument
  }
  'duplicates.copies': { params: VolumeParam & { group_id: string; scope?: PathParam; offset?: number; limit?: number }; result: DuplicateCopiesDocument }
  'duplicates.keep': { params: VolumeParam & { group_id: string; path: PathParam; keep: boolean; scope?: PathParam }; result: DuplicateKeepDocument }
  /** Without `shares`, reads them; with them (at most 64), replaces them. */
  'duplicates.preferred_shares': { params: VolumeParam & { shares?: string[] }; result: DuplicatePreferredDocument }
  'portal.status': { params: NoParams; result: PortalDocument }
  'portal.register': { params: NoParams; result: PortalDocument }
  'portal.unregister': { params: NoParams; result: PortalDocument }
}

export type Method = keyof Methods
export type ParamsOf<M extends Method> = Methods[M]['params']
export type ResultOf<M extends Method> = Methods[M]['result']

/** One way of answering `api()`: the package CGI or recorded fixtures. */
export type Backend = <M extends Method>(method: M, params: ParamsOf<M>, options?: RequestOptions) => Promise<ResultOf<M>>

/**
 * How a caller can give up a request. The DSM backend drops a request that
 * is still queued when its signal aborts, before it takes a CLI slot
 * (design review 2026-10-01 §7.3). It never drops a priority method.
 */
export type RequestOptions = { signal?: AbortSignal }
