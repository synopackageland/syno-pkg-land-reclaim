#!/bin/sh
# Add the Phase 1 extent cases (SPEC 18 "Functional fixtures") under
# $1/extents. POSIX sh; runs on DSM too, where fallocate is missing and
# the prealloc case is skipped. Syncs between steps so extents are
# allocated as described.
set -eu
dir=$1/extents
mkdir "$dir"
cd "$dir"

# Compressed: highly compressible data written with the compression attribute.
mkdir compressed
chattr +c compressed
i=0
while [ "$i" -lt 4096 ]; do
    printf 'synoreclaim compressible line %s .....................................\n' "$i"
    i=$((i + 1))
done > compressed/text.txt
sync

# Reflinks: a copy sharing all extents of its source.
dd if=/dev/urandom of=reflink-src.bin bs=64k count=4 2>/dev/null
sync
cp --reflink=always reflink-src.bin reflink-copy.bin

# Bookend: overwrite the middle of a written extent; the old extent stays
# whole on disk while the file references both ends of it.
dd if=/dev/urandom of=bookend.bin bs=64k count=8 2>/dev/null
sync
dd if=/dev/urandom of=bookend.bin bs=64k count=1 seek=3 conv=notrunc 2>/dev/null
sync

# Preallocated, never written.
if command -v fallocate >/dev/null 2>&1; then
    fallocate -l 256K prealloc.bin
fi
sync
