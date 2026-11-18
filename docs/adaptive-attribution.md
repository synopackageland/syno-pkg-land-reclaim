# Adaptive attribution reads

The memory guard and the metadata I/O allowance are separate. `--pinned-read-budget auto`
selects the maximum of 16,384 read units, used bytes / 8 MiB rounded up, and the
scanned unique extent count multiplied by `1 + min(bit_length(snapshot_count), 8) / 4`.
The extent term is rounded up with saturating arithmetic. Snapshot sharing adds at
most a factor of three, rather than a complete pass per snapshot. A numeric CLI
allowance remains exact, including zero. Results record the inputs and selected
allowance in `stats.pinned_read_plan`.

The allowance counts verified leaf reads and direct-root reference searches, as
before. Extent-tree validation, generation queries and namespace reads are separate
metadata work; it is not a byte limit or a prediction of disk throughput. The
256-leaf cache does not grow with the allowance. Gentle mode retains nice +10,
idle I/O and cooperative pacing.

When the allowance is exhausted, owned extents go into 64 on-disk size buckets.
`scan <volume> --attribution-report <original-job-id>` reuses those references,
starting with the largest byte buckets. Its adaptive allowance also covers the
observed number of deferred backreferences. Each allocation must still have exactly
the saved extent references. Its current file ranges are read again; old namespace
ranges are not combined with new ones. Changed allocations, unreadable leaves and
deleted roots remain separately unknown. Root identities and readonly generations
must match before and after the pass. It does not walk live directories.

Every 256 processed extents, a checksummed progress record and overview are flushed,
fsynced and atomically renamed. Resume validates the immutable queue and reuses
completed chunks; an interrupted chunk is repeated. The overview is display-only:
namespace records, scan times, original coverage and cleanup freshness
are retained. Newly attributed live bytes do not become a new reclaimability proof.
The displayed unaccounted total is updated, and the extra pass records its selected
allowance, actual read units, changed extents, elapsed time and peak RSS.

The 500 TiB synthetic model represents 64 million extents and 4,096 snapshots.
Its admission is 192 million read units; halving used bytes and extent count halves
that admission. A real 65,536-record sample exercises the bucket writer and reader
in an isolated process, retaining only one bounded JSON record and fixed buffers.
This is a bounded-memory and complexity model, not a simulated full NAS scan.
At hypothetical throughput of 2,000 / 20,000 read units per second, spending the
whole admission would take 26.7 / 2.7 hours. Actual reads can be much fewer because
scanned ranges and cached leaves already prove many allocations. Large scans can
therefore run for a long time, and the resumable deep pass is available for them.
