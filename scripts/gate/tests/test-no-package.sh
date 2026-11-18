#!/usr/bin/env bash
# Package skipping is an explicit development-only exception, never a release gate.
set -euo pipefail
gate=$(cd "$(dirname "$0")/../.." && pwd)/gate.sh
if "$gate" --full --skip-package >/dev/null 2>&1; then
  echo 'FAIL: full gate accepted --skip-package'
  exit 1
fi
if "$gate" --clean --skip-package >/dev/null 2>&1; then
  echo 'FAIL: clean accepted --skip-package'
  exit 1
fi
echo 'development-only package skip: pass'
