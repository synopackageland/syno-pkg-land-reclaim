#!/bin/sh
# Add DSM-like service and hidden data (SPEC 4.6) to a fixture volume:
# $1 is the volume mount, $2 a share-like directory below it. Synthetic
# names only; layouts follow what DSM 7.4 showed in a read-only survey.
set -eu
volume=$1
share=$2
blob() {
    mkdir -p "$(dirname "$1")"
    dd if=/dev/urandom of="$1" bs=64k count="$2" 2>/dev/null
}
blob "$volume/@synologydrive/@sync/repo/v1.bin" 4
blob "$volume/@docker/overlay2/layer.bin" 2
blob "$volume/@SYNO.fixture.core.gz" 1
blob "$volume/surveillance/cam-1/20260928AM/rec.mp4" 2
blob "$share/#recycle/old.bin" 3
blob "$share/album/@eaDir/IMG_1.JPG/SYNOPHOTO_THUMB_M.jpg" 1
blob "$share/backup/task.hbk/Pool/0/1.bucket" 2
: > "$share/backup/task.hbk/SynologyHyperBackup.bkpi"
blob "$share/abb/@ActiveBackup/task-1/data.img" 2
