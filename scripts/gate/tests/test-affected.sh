#!/usr/bin/env bash
# Tests affected.sh: the gate parts a fast gate runs for a list of changed paths.
set -euo pipefail
affected=$(dirname "$0")/../affected.sh
failures=0
expect() {
  local want=$1; shift
  local got
  got=$(printf '%s\n' "$@" | "$affected")
  if [[ $got != "$want" ]]; then
    echo "FAIL: $* -> '$got', want '$want'"
    failures=$((failures + 1))
  fi
}
expect "ui" ui/src/App.vue ui/package.json
expect "rust" src/main.rs app/src/lib.rs Cargo.lock tests/golden.rs build.rs
expect "rust" e2e/bridge_test.go fuzz/Cargo.toml
expect "package" package/build-root-programs.sh dsm-package.yaml package-lock.json
expect "rust package" app/permissions.json
expect "ui rust" schemas/v1/scan.json
expect "" README.md docs/notes.md CHANGELOG.md
expect "" </dev/null
expect "ui rust package" scripts/gate/inner.sh
expect "ui rust package" something/unknown.txt
expect "ui rust" ui/src/a.ts src/main.rs package/README.md
if ((failures)); then exit 1; fi
echo "affected.sh: all cases pass"
