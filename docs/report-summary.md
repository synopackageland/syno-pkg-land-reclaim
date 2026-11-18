# Report summary and directory index

Display formats: summary 5, directory index 6, UI answer cache 4. Persisted report
format 4 stays compatible. Other-volume attribution is computed during a fresh
scan; rebuilding these sidecars does not reclassify old per-extent evidence.

Display queries must scale with their answer, rather than with every node in a
volume report. The public JSON schemas stay unchanged. Cleanup plans and apply
never consume this derived state.

## Identity and publication

A report has an adjacent `.srsummary` file: a versioned, checksummed JSON payload
with the report's stored FNV checksum, byte size, device/inode, nanosecond mtime,
and report schema version. A hit verifies the summary checksum, supported
versions, stat identity, and the report's 16-byte trailer. It does not compute the
report's checksum. Each build uses one open report descriptor, checks its identity
again after traversal and against the pathname before publishing. A concurrent
replacement makes the derived result ineligible for reuse.

Summary writes use unique 0600 temporary files in the private report directory,
file sync, atomic rename, and directory sync. Publication is best effort after
the immutable report is renamed: a derived-state failure must not discard a good
scan. Readers reject absent, corrupt, unsupported, truncated, or stale summaries
and build one from the existing checked streaming reader. The successful first
read can answer from the in-memory summary even if persisting it fails. Concurrent
readers may do redundant first builds; unique temp files prevent interference.
The report identity, rather than unrelated report-directory changes, controls
reuse. Pruning a report also removes its derived files.

## Small page answers

The summary stores display header metadata, volume attribution/coverage and
shallow cleanup candidates, each with whether it is a file or a folder. Snapshot
extent holdings and deletion-simulation inputs are excluded from the display
header; why-not-freed retains its cheap live pending-root/free-space probes and
evaluates optional parameters at request time.

The second stage stores snapshot figures per share and per snapshot, without
repeatedly visiting holdings at display time. Held-file limits are applied at
read time. Explicit snapshot selections use the original analysis and retain
existing behavior. Recycle Bin identities and metrics are precomputed using the
same real-bin classifier, including homes and package exclusions. Optional oldest
item queries keep a streaming fallback until a bounded oldest list can answer
exactly. History already uses separate small files and needs no report-body index.

The third stage precomputes default `top.list` answers for the volume and share
roots (all/files/dirs, reclaimable order, at most 200 entries, service and package
semantics preserved). Other sort/filter/limit combinations use the directory
index. Limits up to 200 truncate the stored ranking and corresponding package overlaps;
the complete services summary is independent of the entry limit. The future
Packages page can use the same default volume query and read every service line
(physical, can free, retained, unknown and snapshots not read) and service row
without reading the report body. JSON shape remains unchanged. This is an
internal bound, not a change to CLI output limits.

## Deep Explore

A companion `.srindex` contains sparse 152-byte directory records sorted by
SHA-256 of the lossless path. It retains depth-0/depth-1 roots, subtrees of at least
64 KiB of encoded report records, and small immediate children of any directory
whose subtree exceeds 1 MiB. Other small directories omit both their fixed record
and path blob. A lookup probes the target and then its lossless path ancestors;
the existing TopWalk streams from the nearest retained ancestor and stops after
the target. Every omitted directory has a retained ancestor range of at most
1 MiB: a large parent retains its small children, and a retained small child
bounds all omitted descendants. Very wide directories therefore keep additional
records rather than forcing an unbounded read for an existing omitted directory.
Explicit large-subtree queries still cost their selected subtree; absent targets
and regular files directly in large directories can also require large reads.

Retained records contain the 32-byte digest, fourteen little-endian
u64 fields (start/end/depth, path offset/length, three cached-answer
offset/length/checksum triples), and an 8-byte FNV checksum. A checksummed JSON
header records format, binding, record count and blob length. Only the selected
all/dirs/files answer block is read, rather than decoding all three rankings. Each record stores the report byte range and root depth,
and has its own checksum. A checksummed identity header binds the index to the
same report as the summary. Binary search reads O(log directories) records;
digest collisions are resolved by decoding and comparing the actual root path.
Each touched record's checksum is verified, so display does not checksum the
entire directory index. The summary and index are separately atomic and bound;
an incomplete pair falls back safely. A subtree reader seeks only the selected
ancestor range, checks record decoding and preorder, and reconstructs paths using
that stored ancestor root. Bounds and the root identity are validated before accepting the
answer. The existing TopWalk classifier sees the same absolute paths and relative
preorder below the target; live package lookup remains in the original renderer.
Service/package rows therefore match the original whole-report walk.

Index storage grows with directories and cached answers, not files. Sorting
buffers at most 32,768 records (about 4.75 MiB); binary merge levels use unlinked
private scratch descriptors and bound the number of open runs logarithmically.
Only retained paths/blobs stream to the final index. Pending small-child records
and paths buffer about 4 MiB plus one entry, then spill to one unlinked private
LIFO scratch descriptor. Closing a bounded parent discards its candidate tail
without reading it; closing a large parent retains that tail. Hashing and sorting
are deferred until retention, avoiding work for omitted directories. The open
directory stack holds path lengths into one ancestor-path buffer, so deeply nested
paths do not create a full path copy for every open ancestor.
A checked full report pass builds ranges; default Top
precomputation then makes one root pass and one pass per share, three ranking
heaps together for one target at a time. Each heap compares the measured value
and lossless tie-breaking path against its worst retained entry before copying
candidate paths/package metrics; strictly worse candidates allocate nothing.
Equal keys retain the original heap behavior. It trades one-time publication work for
small page reads. A cached block above 64 MiB is omitted and uses its indexed
subtree instead. Classifier/codec changes must bump the index format. Index
format 3 rejects both the previous dense index (format 2) and pre-VMM-split
answers (format 1) and their stored numeric service IDs;
compatibility rebuilding preserves `vmm_virtual_disks` separately from iSCSI
through the original `service_within` classifier. Supported reports need no
rescan, and the public JSON shape remains unchanged.
Publication failures keep the checked streaming fallback available. Benchmarks include
index bytes, build overhead, and warm display timings with at least five million
nodes, and correctness tests compare original streaming answers byte-for-byte
where timestamps are fixed.

## Directory-heavy benchmark (2026-10-01)

On a Linux build host, Rust 1.97.1 release profile, the model contains 5,505,051 nodes and
505,051 directories: 50 shares, 100 groups per share, 100 small leaf folders per
group, ten files per leaf. Its report is 179,188,137 bytes; the directory ratio,
not the exact NAS report encoding or folder distribution, is modeled on a test NAS.
The previous 5M-node benchmark had only about 2,000 directories and missed this cost.

| Measurement | Dense format 2 | Sparse format 3 |
| --- | ---: | ---: |
| Retained directory records | 505,051 | 5,051 |
| Fixed records (bytes) | 76,767,752 | 767,752 |
| Paths (bytes) | 14,595,502 | 95,502 |
| Cached top blocks (bytes) | 4,613,376 | 4,613,376 |
| Total index (bytes, including header) | 95,976,849 | 5,476,847 |
| Index / report | 53.56% | 3.06% |
| Index build | 15.820 s | 9.015 s |
| Warm volume root (five runs) | 0.155–0.271 ms | 0.138–0.255 ms |
| Warm share | 0.243–0.331 ms | 0.225–0.335 ms |
| Warm group directory | 0.895–0.997 ms | 0.855–0.954 ms |
| Warm omitted leaf directory | 0.150–0.217 ms | 0.663–0.714 ms |

Index size falls 94.29%, below the measured 5% goal; build time falls 43.02%.
The sparse-only intermediate build took 12.564 s; avoiding discarded ranking
allocations reduces it further to 9.015 s. Leaf lookup trades a small bounded
ancestor walk for removing 500,000 records and their paths. Every measured answer
is compared with the original checked streaming walk; ranking pruning is also
checked against an exhaustive sort across metrics, limits, ties and package data.

Run with `SYNORECLAIM_BENCH_DIR=<build dir>/bench cargo +1.97.1 test --release
--lib benchmark_directory_heavy_report -- --ignored --nocapture`, with TMPDIR
inside the build directory. These are synthetic build-host
measurements, not NAS timings or a universal size quota. Very wide directories
retain additional children to preserve the read bound; cached root/share answers
and indexes for retained historical reports still occupy disk.

## Integrity and safety boundary

The full-report FNV checksum formerly detected accidental report corruption or
partial writes even for header-only display. Atomic publication already prevents
partial files; on DSM, Btrfs checksums protect data and the state directory is
owner-only. Recomputing FNV over hundreds of MB for every page cannot prove that
live user files still match an old report. Display therefore checks only the
derived sections it reads and their report binding; a first compatibility rebuild
still validates the original report while streaming it once.

Cleanup reads item sizes from the original reports, never from this
summary/index, and checks each item live before it moves.
Staleness, cleanup marks, partial coverage, unknown bytes, non-UTF-8 paths,
parameter bounds and output schemas retain their existing semantics.

## Delivery

1. Summary writer, Overview and why-not-freed.
2. Snapshot figures and Recycle Bin rows.
3. Root/share top answers and seekable directory offsets.
