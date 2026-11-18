#!/bin/sh
# Populate a stable fixture tree under $1 (an existing, empty directory).
# POSIX sh only: this also runs on DSM (busybox-free GNU userland, no bash
# guarantees). Covers the Phase 0 namespace cases of SPEC 18.
set -eu
root=$1
cd "$root"

mkdir -p docs/deep/er/still empty-dir "spaces in name" "unicodé-目录"
printf 'hello\n' > docs/hello.txt
: > docs/empty.txt
printf 'x' > "spaces in name/one byte"
printf 'utf8 name\n' > "unicodé-目录/файл.txt"
printf 'deep\n' > docs/deep/er/still/leaf

# Regular extents (not inline): 1 MiB of non-zero data.
dd if=/dev/urandom of=docs/random.bin bs=64k count=16 2>/dev/null
# Sparse file with a hole in the middle.
dd if=/dev/urandom of=sparse.bin bs=4k count=1 2>/dev/null
dd if=/dev/urandom of=sparse.bin bs=4k count=1 seek=255 conv=notrunc 2>/dev/null

# Hard links in the same and different directories.
printf 'linked\n' > docs/hard-a
ln docs/hard-a docs/hard-b
ln docs/hard-a "spaces in name/hard-c"

ln -s hello.txt docs/link-to-hello
ln -s /nonexistent/target dangling-link
mkfifo fifo

# Many small files in one directory, enough to span several search batches.
mkdir many
i=0
while [ "$i" -lt 600 ]; do
    printf '%s\n' "$i" > "many/file-$i"
    i=$((i + 1))
done
