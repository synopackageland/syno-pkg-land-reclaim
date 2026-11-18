#!/usr/bin/env bash
# scripts/check-internal-names.sh fails on each generic rule and on names listed
# outside the repository, in the tree, --staged and --message modes, and passes
# clean text. The samples are assembled at run time so this file itself stays clean.
set -euo pipefail
check=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)/scripts/check-internal-names.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
work=$tmp/repo
git init -q "$work"
unset SYNOPKGLAND_INTERNAL_NAMES SYNOPKGLAND_INTERNAL_NAMES_FILE SYNORECLAIM_INTERNAL_NAMES SYNORECLAIM_INTERNAL_NAMES_FILE
printf 'A test NAS at 192.0.2.1, 192.168.1.20 and 10.0.0.5, version 10.1.2; window.SYNO too.\n' >"$work/clean.md"
"$check" "$work" >/dev/null || { echo 'FAIL: a clean tree was refused'; exit 1; }

expect_refused() {
  local label=$1 text=$2
  shift 2
  printf '%s\n' "$text" >"$work/sample.txt"
  if env "$@" "$check" "$work" >"$tmp/out" 2>&1; then
    echo "FAIL: $label was accepted"
    exit 1
  fi
  grep -q 'sample.txt:1:' "$tmp/out" || { echo "FAIL: $label was not located"; cat "$tmp/out"; exit 1; }
}
domain=syno
expect_refused 'a .syno host' "ssh root@box.$domain"
expect_refused 'a company domain' "see https://git.synol""ogy.inc/x"
expect_refused 'a personal domain' "https://packages.var""vis.me/src"
expect_refused 'a 10/8 address' "host 10.$((17)).1.2"
expect_refused 'a 172.16/12 address' "host 172.$((20)).0.9"
expect_refused 'a 192.168/16 address' "host 192.$((168)).50.1"
expect_refused 'a thread id' "fixed in t-$((1000 + 234))"
expect_refused 'a home folder' "/Use""rs/someone/project"
expect_refused 'a GitHub token' "token gh""p_$(printf 'A%.0s' {1..24})"
expect_refused 'a listed name' 'measured on Example-Box' SYNOPKGLAND_INTERNAL_NAMES='other, example-box'
printf 'other # a comment\nexample-box\n' >"$tmp/names"
expect_refused 'a name from the list file' 'measured on example-box' SYNOPKGLAND_INTERNAL_NAMES_FILE="$tmp/names"
printf 'measured on example-boxes\n' >"$work/sample.txt"
SYNOPKGLAND_INTERNAL_NAMES=example-box "$check" "$work" >/dev/null ||
  { echo 'FAIL: a listed name matched inside a longer word'; exit 1; }

# --staged checks only the lines the commit adds.
rm "$work/sample.txt"
git -C "$work" add clean.md
(cd "$work" && "$check" --staged) || { echo 'FAIL: a clean staged diff was refused'; exit 1; }
printf 'fixed in t-%s\n' "$((1000 + 234))" >>"$work/clean.md"
git -C "$work" add clean.md
if (cd "$work" && "$check" --staged) >"$tmp/out" 2>&1; then echo 'FAIL: a staged thread id was accepted'; exit 1; fi
grep -q 'clean.md:2:' "$tmp/out" || { echo 'FAIL: the staged line was not located'; cat "$tmp/out"; exit 1; }

# --message checks the commit message, not its comments.
printf 'Fix a bug\n# t-%s is only a comment\n' "$((1000 + 234))" >"$tmp/msg"
(cd "$work" && "$check" --message "$tmp/msg") || { echo 'FAIL: a comment line was checked'; exit 1; }
printf 'Fix a bug from t-%s\n' "$((1000 + 234))" >"$tmp/msg"
if (cd "$work" && "$check" --message "$tmp/msg") >/dev/null 2>&1; then echo 'FAIL: a message thread id was accepted'; exit 1; fi
echo 'internal-name check: pass'
