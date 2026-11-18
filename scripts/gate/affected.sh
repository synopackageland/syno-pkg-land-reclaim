#!/usr/bin/env bash
# Reads changed paths, one per line, and prints the gate parts a fast gate runs
# for them, in the order ui rust package. Documentation changes nothing; a path
# no rule names, the gate itself included, runs every part.
set -euo pipefail
ui=0 rust=0 package=0
while IFS= read -r path; do
  case $path in
    '' | *.md | docs/*) ;;
    ui/*) ui=1 ;;
    schemas/*) ui=1 rust=1 ;;
    app/permissions.json) rust=1 package=1 ;;
    src/* | app/* | tests/* | fuzz/* | e2e/* | build.rs | Cargo.toml | Cargo.lock) rust=1 ;;
    package/* | dsm-scripts/* | dsm-package.yaml | package.json | package-lock.json) package=1 ;;
    *) ui=1 rust=1 package=1 ;;
  esac
done
parts=()
((ui)) && parts+=(ui)
((rust)) && parts+=(rust)
((package)) && parts+=(package)
echo "${parts[*]-}"
