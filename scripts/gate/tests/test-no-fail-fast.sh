#!/usr/bin/env bash
# Every cargo test in the gate runs with --no-fail-fast, and the checks run the
# App and ctl tests after a failing crate test, so one failure never hides the
# integration results.
set -euo pipefail
inner=$(cd "$(dirname "$0")/.." && pwd)/inner.sh
missing=$(grep -n 'cargo test' "$inner" | grep -v -- '--no-fail-fast' | grep -v '^[0-9]*: *#' || true)
if [[ -n $missing ]]; then
  echo "FAIL: cargo test without --no-fail-fast: $missing"
  exit 1
fi
for line in 'cargo test --locked --no-fail-fast || tests_failed=1' \
  'node --test tests/ctl/*.test.mjs || tests_failed=1' \
  'return "$tests_failed"'; do
  if ! grep -qF -- "$line" "$inner"; then
    echo "FAIL: checks no longer collects test failures ($line)"
    exit 1
  fi
done
echo 'gate tests run to the end: pass'
