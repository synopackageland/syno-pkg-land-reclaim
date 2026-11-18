#!/bin/sh
# Reclaimability invariants and the delete-and-measure oracle (SPEC 18)
# on a disposable Btrfs directory with no other writers.
#
# Creates DIR (which must not exist) on a Btrfs filesystem, builds a
# fixture matrix of exclusive files, a reflink pair, hard links inside and
# across a selection, a bookend extent, a DSM service in two folders
# (one `top` service row), and a subvolume with a read-only
# snapshot and later writes; checks what synoreclaim reports, then deletes
# each selection and compares the Btrfs data-used delta with the reported
# reclaimable bytes. Everything it creates is removed at the end.
#
# Usage: oracle.sh SYNORECLAIM DIR   (root, Linux or DSM; needs jq)
# Environment: UNIT file size in MiB (default 4); TOL allowed delta
# mismatch in bytes (default 0; raise it on a volume with other writers);
# NICE command prefix for synoreclaim (default none); MIN_FREE space the
# state filesystem, DIR's own, must keep (default the tool's 512MiB).
set -eu
bin=$1 dir=$2
unit=${UNIT:-4}
tol=${TOL:-0}
nice=${NICE:-}
min_free=${MIN_FREE:-512MiB}
u=$((unit * 1048576))
failures=0

if [ -e "$dir" ]; then
    echo "oracle: $dir exists; give a new directory" >&2
    exit 2
fi
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
state=$dir/state
m=$dir/m
s=$dir/s

cleanup() {
    for sub in "$s.snap" "$s" "$m"; do
        if [ -d "$sub" ]; then
            btrfs subvolume delete "$sub" >/dev/null 2>&1 || true
        fi
    done
    rm -rf "$state"
    rmdir "$dir" 2>/dev/null || true
}
trap cleanup EXIT

run() {
    # shellcheck disable=SC2086
    $nice "$bin" "$@" --json --quiet --state-dir "$state" --min-free "$min_free" ||
        [ $? -eq 3 ]
}

check() {
    name=$1 actual=$2 expected=$3
    if [ "$actual" = "$expected" ]; then
        echo "PASS $name: $actual"
    else
        echo "FAIL $name: got $actual, expected $expected"
        failures=$((failures + 1))
    fi
}

check_true() {
    name=$1 detail=$2
    shift 2
    if "$@"; then
        echo "PASS $name: $detail"
    else
        echo "FAIL $name: $detail"
        failures=$((failures + 1))
    fi
}

random() {
    dd if=/dev/urandom of="$1" bs=1048576 count="$2" 2>/dev/null
}

commit() {
    sync
    btrfs filesystem sync "$dir" >/dev/null
}

data_used() {
    commit
    commit
    btrfs filesystem df -b "$dir" |
        awk -F'used=' '/^Data/ { split($2, v, /[ ,]/); total += v[1] } END { print total }'
}

# Metric FIELD of PATH from the report covering it.
metric() {
    run inspect "$2" --no-scan | jq -r ".results[0].entry.metrics.$1"
}

blockers() {
    run inspect "$1" --no-scan | jq -r '.results[0].entry.blockers | join(",")'
}

# FIELD of the top row of service CODE below PATH.
service_row() {
    run top "$3" --no-scan |
        jq -r ".results[0].entries[] | select(.service.id == \"$2\") | $1"
}

# Combined reclaimable bytes of deleting all PATHs together.
together() {
    run simulate-delete "$@" | jq -r '.results[0].combined.reclaimable'
}

bonus() {
    run simulate-delete "$@" | jq -r '.results[0].together_bonus'
}

# Deletes PATHs and checks the data-used delta against EXPECTED bytes.
delete_and_measure() {
    name=$1 expected=$2
    shift 2
    before=$(data_used)
    rm -rf "$@"
    after=$(data_used)
    freed=$((before - after))
    diff=$((freed - expected))
    [ "$diff" -lt 0 ] && diff=$((-diff))
    if [ "$diff" -le "$tol" ]; then
        echo "PASS oracle $name: freed $freed, reported $expected"
    else
        echo "FAIL oracle $name: freed $freed, reported $expected (tolerance $tol)"
        failures=$((failures + 1))
    fi
}

contains() {
    case ",$1," in *",$2,"*) return 0 ;; esac
    return 1
}

between() {
    [ "$1" -gt "$2" ] && [ "$1" -lt "$3" ]
}

positive() {
    [ "$1" -gt 0 ]
}

# The matrix: m is never snapshotted, s has a read-only snapshot.
btrfs subvolume create "$m" >/dev/null
random "$m/excl.bin" "$unit"
mkdir "$m/pair" "$m/hl" "$m/hx" "$m/hx/in" "$m/hx/out"
random "$m/pair/a.bin" "$unit"
commit
cp --reflink=always "$m/pair/a.bin" "$m/pair/b.bin"
random "$m/hl/h1" "$unit"
ln "$m/hl/h1" "$m/hl/h2"
random "$m/hx/in/x" "$unit"
ln "$m/hx/in/x" "$m/hx/out/x"
random "$m/book.bin" $((unit * 2))
# Hyper Backup data in two folders: one service row of top. A link to a
# file outside it holds that file's data.
mkdir -p "$m/svc/a/backup.hbk" "$m/svc/b/backup.hbk"
random "$m/svc/a/backup.hbk/t" "$unit"
random "$m/svc/b/backup.hbk/t" "$unit"
random "$m/svc/keep" "$unit"
ln "$m/svc/keep" "$m/svc/b/backup.hbk/l"
commit
dd if=/dev/urandom of="$m/book.bin" bs=1048576 count=1 seek=1 conv=notrunc 2>/dev/null

btrfs subvolume create "$s" >/dev/null
random "$s/old.bin" "$unit"
random "$s/mod.bin" "$unit"
commit
btrfs subvolume snapshot -r "$s" "$s.snap" >/dev/null
dd if=/dev/urandom of="$s/mod.bin" bs=1048576 count=1 conv=notrunc 2>/dev/null
random "$s/new.bin" "$unit"
commit

run scan "$m" >/dev/null
run scan "$s" >/dev/null

echo "== invariants"
check "exclusive file is reclaimable" "$(metric reclaimable "$m/excl.bin")" "$u"
check "exclusive file has no blockers" "[$(blockers "$m/excl.bin")]" "[]"
check "one reflink clone frees nothing" "$(metric reclaimable "$m/pair/a.bin")" 0
check_true "clone is held by a live reflink" "$(blockers "$m/pair/a.bin")" \
    contains "$(blockers "$m/pair/a.bin")" live_reflink
check "both clones free the extent once" "$(metric reclaimable "$m/pair")" "$u"
check "clones share one physical extent" "$(metric physical "$m/pair")" "$u"
pair_together=$(together "$m/pair/a.bin" "$m/pair/b.bin")
check "clones selected together free the extent" "$pair_together" "$u"
check "together bonus exceeds the sum" "$(bonus "$m/pair/a.bin" "$m/pair/b.bin")" "$u"
check "one hard link name frees nothing" "$(metric reclaimable "$m/hl/h1")" 0
check_true "name is held by a hard link" "$(blockers "$m/hl/h1")" \
    contains "$(blockers "$m/hl/h1")" hard_link
check "the last selected name frees the data" "$(metric reclaimable "$m/hl")" "$u"
check "a link outside the selection retains" "$(metric reclaimable "$m/hx/in")" 0
hx_together=$(together "$m/hx/in" "$m/hx/out")
check "both links together free the data" "$hx_together" "$u"
book=$(metric reclaimable "$m/book.bin")
check "a bookend file frees its whole extents" "$book" "$(metric physical "$m/book.bin")"
check_true "bookend pins unreferenced bytes" "$(metric pinned_unreferenced "$m/book.bin")" \
    positive "$(metric pinned_unreferenced "$m/book.bin")"
check "snapshot-held file frees nothing" "$(metric reclaimable "$s/old.bin")" 0
check_true "file is held by a snapshot" "$(blockers "$s/old.bin")" \
    contains "$(blockers "$s/old.bin")" snapshot
check "file written after the snapshot is reclaimable" "$(metric reclaimable "$s/new.bin")" "$u"
mod=$(metric reclaimable "$s/mod.bin")
check_true "modified file frees only rewritten extents" "$mod of $(metric physical "$s/mod.bin")" \
    between "$mod" 0 "$(metric physical "$s/mod.bin")"
check "no unknown bytes in the matrix" "$(metric unknown "$m")+$(metric unknown "$s")" "0+0"
s_together=$(together "$s/old.bin" "$s/mod.bin" "$s/new.bin")
svc=$(service_row .metrics.reclaimable hyper_backup "$m/svc")
check "a service row adds up its paths" "$svc" "$((u * 2))"
check "a service row names its paths" "$(service_row .service.path_count hyper_backup "$m/svc")" 2
check "a service row is never a cleanup target" \
    "$(service_row .cleanup_eligible hyper_backup "$m/svc")" false
check "a link outside the service holds its data" \
    "$(service_row '.held_by | map("\(.class)=\(.bytes)") | join(",")' hyper_backup "$m/svc")" \
    "hard_link=$u"

echo "== delete and measure"
delete_and_measure "exclusive file" "$u" "$m/excl.bin"
delete_and_measure "one reflink clone" 0 "$m/pair/a.bin"
delete_and_measure "the other clone" "$((pair_together))" "$m/pair/b.bin"
delete_and_measure "hard-linked directory" "$u" "$m/hl"
delete_and_measure "one of two links" 0 "$m/hx/in/x"
delete_and_measure "the last link" "$hx_together" "$m/hx/out/x"
delete_and_measure "bookend file" "$book" "$m/book.bin"
delete_and_measure "a service row's paths" "$svc" "$m/svc/a/backup.hbk" "$m/svc/b/backup.hbk"
delete_and_measure "snapshotted subvolume files" "$s_together" \
    "$s/old.bin" "$s/mod.bin" "$s/new.bin"

if [ "$failures" -gt 0 ]; then
    echo "oracle: $failures failure(s)"
    exit 1
fi
echo "oracle: all checks passed"
