#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
cd -- "$repo_root"
exec python3 -B scripts/ui-fixtures/record.py "$@"
