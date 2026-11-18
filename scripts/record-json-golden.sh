#!/bin/sh
# Record the JSON golden outputs checked by tests/schema.rs from the loop
# fixture (synthetic names only). The fixture defaults to /fixture so
# recorded paths are stable; FIXTURE_DIR can keep all writes in a private
# build workspace. Usage: record-json-golden.sh SYNORECLAIM OUTDIR
# (root, Linux; replaces OUTDIR/*.json).
set -eu
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
out=$(mkdir -p "$2" && cd "$2" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
work=${FIXTURE_DIR:-/fixture}
state=$work/state
recycle_state=$work/recycle-state
trap '"$here/fixture/teardown.sh" "$work"; rm -rf "$state" "$recycle_state"; rmdir "$work" 2>/dev/null || true' EXIT
mnt=$("$here/fixture/build.sh" "$work")
data=$mnt/data

# Exit codes 0 and 3 (partial) are expected.
record() {
    name=$1
    shift
    "$bin" "$@" > "$out/$name.json" || [ $? -eq 3 ]
}
s="--json --quiet --state-dir $state"
record scan scan "$data" $s
record scan-volume scan "$mnt" --scope volume $s
record overview overview "$mnt" --no-scan $s
record why-not-freed why-not-freed "$mnt" --since 1d $s
record top-volume top "$mnt" --limit 4 --no-scan $s
record scan-partial scan /proc "$data" $s
record top top "$data" --kind files --limit 4 --include-system --no-scan $s
record top-held top "$data" --by held --limit 6 --no-scan $s
record inspect inspect "$data/extents/reflink-copy.bin" "$mnt/nope" --explain --no-scan $s
record simulate-delete simulate-delete "$data/extents/reflink-src.bin" \
    "$data/extents/reflink-copy.bin" --explain $s
record shared shared "$data" --kind files --limit 4 --no-scan $s
record volumes volumes --json --state-dir "$state"
record cache cache status "$mnt" $s
record jobs jobs --json --state-dir "$state"
record history history --json --state-dir "$state"
# A stopped job: pause refuses (exit 1), cancel ends it.
stopped=$("$bin" scan "$data" --max-runtime 0s $s || [ $? -eq 10 ])
job=$(printf '%s\n' "$stopped" | sed -n 's/^  "job_id": "\(.*\)",$/\1/p')
"$bin" pause "$job" --json --state-dir "$state" > "$out/pause.json" || [ $? -eq 1 ]
record cancel cancel "$job" --json --state-dir "$state"
"$bin" resume "$job" $s > "$out/resume.json" || [ $? -eq 1 ]
# Queued for an external launcher; never run, so it changes no report.
record detach scan "$data" --detach=none --state-dir "$state"
# Last, so the report it adds answers none of the commands above.
"$bin" scan "$data" --snapshots $s > /dev/null
record snapshots snapshots "$data" --simulate-delete all $s
# watch-free takes no state dir; it samples the mounted volume for 2 s.
"$bin" watch-free "$mnt" --duration 2s --interval 2s --json > "$out/watch-free.json" || [ $? -eq 3 ]

# Create this direct-child subvolume after the volume golden is recorded,
# so the new fixture does not alter earlier outputs.
recycle_share=$mnt/recycle-fixture
btrfs subvolume create "$recycle_share" >/dev/null
mkdir "$recycle_share/#recycle"
head -c 4194304 /dev/urandom > "$recycle_share/#recycle/freed.bin"
head -c 4194304 /dev/urandom > "$recycle_share/#recycle/held.bin"
ln "$recycle_share/#recycle/held.bin" "$recycle_share/live-held.bin"
btrfs filesystem sync "$mnt"
record recycle recycle "$recycle_share" --refresh --json --quiet --state-dir "$recycle_state"

# Keep checked-in paths stable even when the temporary fixture lives in a
# per-thread build directory.
for file in "$out"/*.json; do
    sed -i "s|$bin|/usr/bin/synoreclaim|g; s|$work|/fixture|g" "$file"
done
