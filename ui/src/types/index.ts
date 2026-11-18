// Short names for the CLI documents the UI reads (generated from
// schemas/v1 into ./schema by `pnpm gen:types`).
export type { Envelope, Notice, Path, ScanHint, TargetError } from './schema/volumes'
import type { SynoreclaimCleanupPlanResultV1 } from './schema/cleanup-plan'
export type CleanupPlanDocument = SynoreclaimCleanupPlanResultV1 & { command: 'cleanup-plan' }
export type { SynoreclaimCacheResultV1 as CacheDocument } from './schema/cache'
export type { SynoreclaimCancelResultV1 as CancelDocument } from './schema/cancel'
import type { SynoreclaimCleanupApplyResultV1 } from './schema/cleanup-apply'
export type CleanupApplyDocument = SynoreclaimCleanupApplyResultV1 & { command: 'cleanup-apply' }
export type { SynoreclaimScanDetachResultV1 as DetachDocument } from './schema/detach'
export type { SynoreclaimHistoryResultV1 as HistoryDocument } from './schema/history'
export type { SynoreclaimInspectResultV1 as InspectDocument } from './schema/inspect'
export type { SynoreclaimJobsResultV1 as JobsDocument } from './schema/jobs'
export type { SynoreclaimOverviewResultV1 as OverviewDocument } from './schema/overview'
export type { SynoreclaimPauseResultV1 as PauseDocument } from './schema/pause'
export type { SynoreclaimModeResultV1 as ModeDocument } from './schema/mode'
export type { SynoreclaimRecycleResultV1 as RecycleDocument } from './schema/recycle'
export type { SynoreclaimRootfsApplyResultV1 as RootfsApplyDocument } from './schema/rootfs-apply'
export type { RootfsApplySuccess as RootfsApplySuccessDocument } from './schema/rootfs-apply'
import type { SynoreclaimRootfsReportV1 } from './schema/rootfs'
export type RootfsDocument = SynoreclaimRootfsReportV1 & { command: 'rootfs' }
export type { RootfsPlanSuccess as RootfsPlanDocument, SynoreclaimRootfsPlanResultV1 as RootfsPlanResultDocument } from './schema/rootfs-plan'
export type { SynoreclaimRootfsHistoryResultV1 as RootfsHistoryDocument } from './schema/rootfs-history'
export type { RootfsReportSuccess as RootfsReportDocument } from './schema/rootfs'
export type { SynoreclaimScanResultV1 as ScanDocument } from './schema/scan'
export type { SynoreclaimWebapiSessionGetV1 as SessionDocument } from './schema/session'
export type { SynoreclaimSimulateDeleteResultV1 as SimulateDocument } from './schema/simulate-delete'
export type { SynoreclaimSnapshotsResultV1 as SnapshotsDocument } from './schema/snapshots'
export type { SynoreclaimTopResultV1 as TopDocument } from './schema/top'
export type { SynoreclaimVolumesResultV1 as VolumesDocument } from './schema/volumes'
export type { SynoreclaimWatchFreeResultV1 as WatchFreeDocument } from './schema/watch-free'
export type { SynoreclaimWhyNotFreedResultV1 as WhyNotFreedDocument } from './schema/why-not-freed'

import type { SynoreclaimVolumesResultV1 } from './schema/volumes'
import type { SynoreclaimJobsResultV1 } from './schema/jobs'

export type Volume = SynoreclaimVolumesResultV1['volumes'][number]
export type Job = SynoreclaimJobsResultV1['jobs'][number]
