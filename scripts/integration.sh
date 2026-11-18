#!/bin/sh
# Layer-2 integration tests (SPEC 18, run natively on a Linux host as
# root): build a loop-mounted Btrfs fixture, run the ignored live tests
# and the delete-and-measure oracle against it, and always tear the
# fixture down.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
workdir=${FIXTURE_DIR:-$here/../fixtures}
trap '"$here/fixture/teardown.sh" "$workdir"' EXIT
mnt=$("$here/fixture/build.sh" "$workdir")
SYNORECLAIM_FIXTURE=$mnt cargo +1.97.1 test --test live -- --ignored --test-threads=1 "$@"
# The fixture is smaller than the default free-space reserve.
MIN_FREE=16MiB "$here/oracle.sh" "$here/../target/debug/synoreclaim" "$mnt/oracle"
