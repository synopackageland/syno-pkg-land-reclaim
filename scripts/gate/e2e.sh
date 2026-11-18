#!/usr/bin/env bash
# Build the release App and run the SDK bridge E2E as the package user.
# Usage: e2e.sh <work directory containing src/ and sdk.git/>
set -euo pipefail
work=$1
cd "$work/src"

CARGO_TARGET_DIR=$PWD/target/app cargo build --manifest-path app/Cargo.toml --locked --release
cp /root/.gitconfig /tmp/.gitconfig && chmod 644 /tmp/.gitconfig
# Open the archived source, not the gate's own scratch: the other jobs build
# under target/ and their tests write under .tmp/ while this runs, so a
# recursive chmod there races files that come and go and fails on test
# symlinks left dangling (a /proc/<pid> link after its process). Symlinks are
# skipped everywhere: chmod follows them, and what they point to in the tree
# is opened on its own. Then only the path to the release App the package
# user runs.
find "$work/src" \( -path "$work/src/target" -o -path "$work/src/.tmp" \) -prune \
  -o ! -type l -exec chmod a+rX {} +
chmod -R a+rX "$work/sdk.git"
app=target/app/release/synoreclaim-app
chmod a+rX target target/app target/app/release "$app"
# The archive preserves mktemp's 0700 parent. The package user needs to
# traverse it to reach the readable source, SDK and release App below it.
chmod a+x "$work"
setpriv --reuid=1000 --regid=1000 --clear-groups env HOME=/tmp \
  RECLAIM_APP="$PWD/$app" e2e/run.sh -v
