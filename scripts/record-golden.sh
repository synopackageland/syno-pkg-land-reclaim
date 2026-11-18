#!/bin/sh
# Record golden TREE_SEARCH_V2 buffers from a Btrfs directory for
# tests/golden.rs: NAME.srbuf (sanitised calls), NAME.meta (tree/root
# identity), and NAME.listing (find enumeration, the independent oracle).
# The raw recording names every subvolume on the host; only the output of
# `debug sanitise` is kept. DIR must be a synthetic fixture: its own file
# names are recorded as they are.
# Usage: record-golden.sh SYNORECLAIM DIR OUTDIR NAME   (root, Linux)
set -eu
bin=$1 dir=$2 out=$3 name=$4
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
raw=$out/$name.raw.srbuf
rm -f "$raw" "$out/$name.srbuf"
trap 'rm -f "$raw"' EXIT
"$bin" debug record "$dir" -o "$raw" > "$out/$name.meta"
"$bin" debug sanitise "$raw" -o "$out/$name.srbuf"
"$here/fixture/listing.sh" "$dir" > "$out/$name.listing"
