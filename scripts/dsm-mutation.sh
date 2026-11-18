#!/bin/sh
# Mutation check for generation-assisted reuse (SPEC 12). After each kind of
# namespace change in a fresh subvolume: the generation must advance, the
# metadata listing (`synoreclaim debug tree`) must converge to what
# directory enumeration lists (delayed allocation updates sizes only at
# writeback, in a later transaction), and a settled namespace must keep its
# generation and listing while idle, which is what reuse relies on. Tree
# search also sees changes not committed yet, so a generation can show two
# listings until the next commit; those are counted, not failed. Each check
# commits pending changes first (`btrfs filesystem sync`), so its mutation
# lands in a transaction after the recorded generation: a newer kernel no
# longer commits on `btrfs subvolume create`, and on a relatime mount the
# enumeration's own directory reads update atimes that the next commit
# writes, up to a commit interval later. A DSM build
# passing every check may be added to VALIDATED_DSM_BUILDS. Usage:
# dsm-mutation.sh SYNORECLAIM DIR (root; creates and deletes the subvolume
# DIR/mutation and a work directory beside it, nothing else).
set -eu
bin=$1
sub=$2/mutation
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "$2/mutation-work.XXXXXX")
btrfs subvolume create "$sub" > /dev/null
trap 'btrfs subvolume delete "$sub/snap" > /dev/null 2>&1 || true
    btrfs subvolume delete "$sub" > /dev/null; rm -rf "$work"' EXIT
failed=0

generation() {
    btrfs subvolume show "$sub" | awk '$1 == "Generation:" { print $2; exit }'
}

# Commits everything written so far, including atime updates.
commit() {
    btrfs filesystem sync "$sub" > /dev/null
}

# Lists the metadata of one generation into $work/metadata, recording
# "generation digest" in $work/samples; fails when a commit raced it.
sample() {
    g=$(generation)
    nice -n 19 "$bin" debug tree "$sub" 2> /dev/null > "$work/metadata"
    [ "$(generation)" = "$g" ] || return 1
    echo "$g $(md5sum < "$work/metadata" | cut -d' ' -f1)" >> "$work/samples"
}

# Samples until no commit races the listing.
sample_settled() {
    until sample; do
        sleep 1
    done
}

# Whether the last sample is past generation $1 and lists what directory
# enumeration does.
settled() {
    [ "$g" -gt "$1" ] || return 1
    "$here/fixture/listing.sh" "$sub" > "$work/enumerated"
    cmp -s "$work/metadata" "$work/enumerated"
}

# check NAME COMMAND...: runs the mutation, then samples for up to 90 s
# (several commit and writeback intervals) until the listings agree.
check() {
    name=$1
    shift
    commit
    before=$(generation)
    if ! "$@" > /dev/null 2> "$work/err"; then
        echo "SKIP $name: $(head -1 "$work/err")"
        return
    fi
    for second in $(seq 90); do
        if sample && settled "$before"; then
            echo "PASS $name: generation $before -> $g, settled in ${second}s"
            return
        fi
        sleep 1
    done
    if [ "$(generation)" -le "$before" ]; then
        echo "FAIL $name: generation stayed at $before for 90s"
    else
        echo "FAIL $name: generation $before -> $(generation), listings differ after 90s"
        diff "$work/metadata" "$work/enumerated" | head -5 || true
    fi
    failed=1
}

f=$sub/file
check create sh -c "printf x > '$f'"
check append sh -c "head -c 70000 /dev/urandom >> '$f'"
check overwrite dd if=/dev/urandom of="$f" bs=4096 count=1 conv=notrunc status=none
check truncate truncate -s 10 "$f"
check mkdir mkdir "$sub/dir"
check rename-across mv "$f" "$sub/dir/moved"
check rename-within mv "$sub/dir/moved" "$sub/dir/renamed"
check hardlink ln "$sub/dir/renamed" "$sub/link"
check symlink ln -s dir/renamed "$sub/symlink"
check unlink rm "$sub/link"
check chmod chmod 600 "$sub/dir/renamed"
check reflink cp --reflink=always "$sub/dir/renamed" "$sub/clone"
check snapshot btrfs subvolume snapshot "$sub" "$sub/snap"
check snapshot-delete btrfs subvolume delete "$sub/snap"
check rmdir rm -r "$sub/dir"
# Settled and idle: the generation and the listing stay put. The last
# check's enumeration dirtied atimes; commit them first.
commit
sample_settled
idle=$g
cp "$work/metadata" "$work/idle"
sleep 35
sample_settled
if [ "$g" = "$idle" ] && cmp -s "$work/metadata" "$work/idle"; then
    echo "PASS idle: generation $g and its listing unchanged for 35s"
else
    echo "FAIL idle: without a mutation, generation $idle -> $g; listing changes:"
    diff "$work/idle" "$work/metadata" | head -5 || true
    failed=1
fi
split=$(sort -u "$work/samples" | awk '{ n[$1]++ } n[$1] == 2 { print $1 }' | wc -l)
echo "INFO $(wc -l < "$work/samples") samples; $split generations showed an uncommitted change"
if [ "$failed" -ne 0 ]; then
    echo "dsm-mutation: FAILED"
    exit 1
fi
echo "dsm-mutation: all checks passed"
