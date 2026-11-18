#!/bin/sh
# Build a loop-mounted Btrfs fixture image under WORKDIR (default:
# ./fixtures) and print the mount point. Linux, root only. Layout:
#   <mnt>/data            subvolume with the populate.sh tree and the
#                         populate-extents.sh cases under data/extents
#   <mnt>/data/nested     nested subvolume (a boundary inside data)
#   <mnt>/snap            read-only snapshot of data
#   DSM-like service data from populate-dsm.sh: @synologydrive, @docker
#   and surveillance at the root; #recycle, @eaDir, a .hbk target and
#   an @ActiveBackup destination under data
# Tear down with teardown.sh.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
workdir=$(mkdir -p "${1:-./fixtures}" && cd "${1:-./fixtures}" && pwd)
img=$workdir/fixture.img
mnt=$workdir/mnt

truncate -s 512M "$img"
mkfs.btrfs -q -f "$img"
mkdir -p "$mnt"
mount -o loop "$img" "$mnt"

btrfs -q subvolume create "$mnt/data"
"$here/populate.sh" "$mnt/data"
"$here/populate-extents.sh" "$mnt/data"
btrfs -q subvolume create "$mnt/data/nested"
printf 'inside nested\n' > "$mnt/data/nested/file"
"$here/populate-dsm.sh" "$mnt" "$mnt/data"
btrfs -q subvolume snapshot -r "$mnt/data" "$mnt/snap"
btrfs filesystem sync "$mnt"
echo "$mnt"
