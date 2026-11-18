#!/usr/bin/env bash
# Usage: scripts/gate.sh [--full | --fast [--skip-package] | --clean]
# Runs the repository's gate on the working tree, untracked files included.
#   --full (default)  every part, as before a release: the UI here, and the Rust
#                     gate with the qemu aarch64 and armv7 tests, e2e/ and the
#                     SPK of every architecture in the gate image on Docker.
#   --fast            only the parts the paths changed since origin/main touch
#                     (scripts/gate/affected.sh), without the qemu tests; it
#                     prints what it skips and why. The full gate runs them all.
#   --clean           removes this tree's container, work folder and target
#                     volume from the Docker host.
# The rust and package parts run on SYNOPKGLAND_DOCKER_SSH=<ssh host> (local
# Docker when unset), with the SDK that app/Cargo.toml pins cloned from
# SYNOPKGLAND_SDK_DIR. The UI part runs there too, so this machine's load cannot
# slow its tests; without SYNOPKGLAND_DOCKER_SSH it runs pnpm in ui/ here. The parts run at
# once, each with its own log in dist/gate/; the SPKs land in dist/.
# SYNOPKGLAND_DOCKER_NPM_REGISTRY: the registry npm uses on the Docker host.
# SYNOPKGLAND_CATALOG: the schema 4 catalog file that holds the store text; the
# package part builds and inspects with a copy of it and returns the build's
# release-notes.md to dist/ with the SPKs.
# GATE_ID (default reclaim-<hash of this tree's path>) names the container,
# work folder and target volume; GATE_JOBS caps the parallel Docker sections.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=full
skip_package=0
case ${1:-} in
  '' | --full) ;;
  --fast) mode=fast ;;
  --clean) mode=clean ;;
  *) sed -n '2,10p' "$0" >&2; exit 2 ;;
esac
if [[ ${2:-} == --skip-package && $mode == fast && $# == 2 ]]; then
  skip_package=1
elif (( $# > 1 )); then
  echo "--skip-package is supported only with --fast" >&2
  exit 2
fi
host=${SYNOPKGLAND_DOCKER_SSH:-}
id=${GATE_ID:-reclaim-$(pwd -P | git hash-object --stdin | cut -c1-8)}
case $id in *[!a-z0-9_.-]* | '') echo "invalid GATE_ID: $id" >&2; exit 2 ;; esac
node_on_host=${SYNOPKGLAND_NODE_ON_HOST:-0}
registry=${SYNOPKGLAND_DOCKER_NPM_REGISTRY:-}
case $registry in *[!A-Za-z0-9:/._-]*) echo "invalid SYNOPKGLAND_DOCKER_NPM_REGISTRY: $registry" >&2; exit 2 ;; esac
jobs=${GATE_JOBS:-}
case $jobs in *[!0-9]*) echo "invalid GATE_JOBS: $jobs" >&2; exit 2 ;; esac
dir=/tmp/$id-gate
# shellcheck disable=SC2029 # the commands are meant for the Docker host
on_host() { if [[ -n $host ]]; then ssh "$host" "$@"; else bash -c "$*"; fi; }

if [[ $mode == clean ]]; then
  # rm -f returns at once for a --rm container Docker is still removing, and
  # the volume stays in use until it is gone: wait up to 30 s for it.
  on_host "docker rm -f $id-gate >/dev/null 2>&1
    for _ in \$(seq 60); do docker container inspect $id-gate >/dev/null 2>&1 || break; sleep 0.5; done
    if docker volume inspect $id-target >/dev/null 2>&1; then docker volume rm $id-target >/dev/null; fi
    rm -rf $dir" || true
  left=$(on_host "docker container inspect $id-gate >/dev/null 2>&1 && echo $id-gate
    docker volume inspect $id-target >/dev/null 2>&1 && echo $id-target
    test -e $dir && echo $dir" || true)
  if [[ -n $left ]]; then
    echo "still there: ${left//$'\n'/ }" >&2
    exit 1
  fi
  echo "removed $id-gate, $id-target and $dir"
  exit 0
fi

scripts/gate/tests/test-affected.sh
scripts/gate/tests/test-clean.sh
bash scripts/gate/tests/test-no-package.sh
scripts/gate/tests/test-no-fail-fast.sh
scripts/gate/tests/test-internal-names.sh
# Internal host names, private addresses, thread ids and home folders stay out
# of the repository (machine names come from outside it; see the script).
scripts/check-internal-names.sh

# The tree under test: the working tree with untracked files, as a git tree.
index=$(mktemp)
cp "$(git rev-parse --git-path index)" "$index"
GIT_INDEX_FILE=$index git add -A
tree=$(GIT_INDEX_FILE=$index git write-tree)
rm -f "$index"

if [[ $mode == full ]]; then
  parts="ui rust package"
else
  base=$(git merge-base origin/main HEAD)
  changed=$(git diff --name-only "$base" "$tree")
  parts=$(printf '%s\n' "$changed" | scripts/gate/affected.sh)
  echo "fast gate: $(printf '%s' "$changed" | grep -c '^' || true) paths changed since origin/main (${base:0:8})"
  for part in ui rust package; do
    [[ " $parts " == *" $part "* ]] || echo "skip $part: no changed path is one it checks (scripts/gate/affected.sh)"
  done
fi
if ((skip_package)); then
  parts=$(printf '%s\n' "$parts" | sed -E 's/(^| )package( |$)/ /g; s/^ +//; s/ +$//')
  echo "skip package: explicit --fast --skip-package; no SPK is built or inspected"
fi
echo "gate ($mode): ${parts:-nothing to run}"
[[ -n $parts ]] || exit 0

out=dist/gate
rm -rf "$out"
mkdir -p "$out"

ui_gate() {
  pnpm -C ui install --frozen-lockfile
  pnpm -C ui lint
  pnpm -C ui gen:types
  git diff --exit-code -- ui/src/types/schema
  pnpm -C ui typecheck
  pnpm -C ui test
  pnpm -C ui build
  pnpm -C ui build:dsm
  pnpm -C ui check:dsm
}

docker_gate() {
  local work rc=0
  : "${SYNOPKGLAND_SDK_DIR:?set SYNOPKGLAND_SDK_DIR to an SDK checkout holding the commit app/Cargo.toml pins}"
  work=$(mktemp -d)
  mkdir "$work/src"
  git archive "$tree" | tar -x -C "$work/src"
  if [[ $node_on_host == 1 ]]; then
    npm ci --no-audit --no-fund
    pnpm -C ui install --frozen-lockfile
    # The package step runs synopkgland build, whose esbuild is native: install
    # the root dependencies for Linux from the same lockfile, not this host's.
    npm ci --prefix "$work/src" --os=linux --cpu=x64 --libc=glibc --ignore-scripts --no-audit --no-fund
    cp -R ui/node_modules "$work/src/ui/node_modules"
    # The package UI build runs on Linux; fetch its locked optional native
    # dependencies here as well, without changing the App's lockfile or pins.
    cat >"$work/src/ui/pnpm-workspace.yaml" <<'ARCHES'
supportedArchitectures:
  os: [current, linux]
  cpu: [current, x64]
  libc: [glibc]
ARCHES
    pnpm --dir "$work/src/ui" install --frozen-lockfile --force
    rm "$work/src/ui/pnpm-workspace.yaml"
  fi
  git clone -q --bare "$SYNOPKGLAND_SDK_DIR" "$work/sdk.git"
  if [[ -n ${SYNOPKGLAND_CATALOG:-} ]]; then cp "$SYNOPKGLAND_CATALOG" "$work/catalog.yaml"; fi
  COPYFILE_DISABLE=1 tar --no-xattrs -c -C "$work" . | on_host "rm -rf $dir && mkdir -p $dir && tar -x -C $dir"
  rm -rf "$work"
  on_host "PREPARED_NODE=$node_on_host NPM_REGISTRY=$registry GATE_JOBS=$jobs bash $dir/src/scripts/gate/host.sh $dir $id $mode $1" || rc=$?
  mkdir -p "$out/docker"
  on_host "cd $dir && tar -c logs \$(test -d src/dist && echo src/dist)" | tar -x -C "$out/docker" || true
  on_host "rm -rf $dir"
  if compgen -G "$out/docker/src/dist/*.spk" >/dev/null; then mv "$out"/docker/src/dist/*.spk dist/; fi
  if [[ -f $out/docker/src/dist/release-notes.md ]]; then mv "$out/docker/src/dist/release-notes.md" dist/; fi
  return "$rc"
}

start=$SECONDS
names=() pids=()
docker_parts=$parts
if [[ ( -z $host || $node_on_host == 1 ) && " $parts " == *" ui "* ]]; then
  ui_gate >"$out/ui.log" 2>&1 &
  names+=(ui) pids+=($!)
  docker_parts=${parts#ui}
  docker_parts=${docker_parts# }
fi
if [[ -n $docker_parts ]]; then
  docker_gate "$docker_parts" >"$out/docker.log" 2>&1 &
  names+=(docker) pids+=($!)
fi

failed=()
for i in "${!names[@]}"; do
  if wait "${pids[i]}"; then
    echo "ok   ${names[i]} ($out/${names[i]}.log)"
  else
    echo "FAIL ${names[i]} ($out/${names[i]}.log)"
    failed+=("${names[i]}")
  fi
done
if [[ -f $out/docker.log ]]; then grep -E '^(ok |FAIL|skip|gate slot|waiting|building)' "$out/docker.log" | sed 's/^/  docker: /' || true; fi
for name in "${failed[@]+"${failed[@]}"}"; do
  echo "===== $out/$name.log"
  cat "$out/$name.log"
  if [[ $name == docker && -f $out/docker/logs/status ]]; then
    while read -r section rc _; do
      if [[ $rc != 0 ]]; then
        echo "===== $out/docker/logs/$section.log"
        cat "$out/docker/logs/$section.log"
      fi
    done <"$out/docker/logs/status"
  fi
done
echo "gate ($mode): $((SECONDS - start)) s, ${#failed[@]} failed part(s)"
((${#failed[@]} == 0))
