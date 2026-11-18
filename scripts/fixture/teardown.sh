#!/bin/sh
# Unmount and delete a fixture created by build.sh in WORKDIR.
set -eu
workdir=${1:-./fixtures}
mnt=$workdir/mnt
if mountpoint -q "$mnt"; then
    umount "$mnt"
fi
rm -f "$workdir/fixture.img"
rmdir "$mnt" 2>/dev/null || true
