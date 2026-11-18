#!/usr/bin/env bash
# Usage: host.sh <dir> <id> <fast|full> <part>...
# Runs on the Docker host for scripts/gate.sh, with the tree in <dir>/src and a
# bare SDK clone in <dir>/sdk.git. It waits for one of the host's two gate
# slots (flock on /tmp/synopkgland-gate.slot{0,1}.lock), builds the gate image
# synopkgland-gate-reclaim:<first 12 hex digits of the Dockerfile's SHA-256>
# when it is missing, and runs inner.sh in the container <id>-gate. Only that
# missing tag is built: the shared synopkgland-* images and caches are never
# removed or overwritten, so another tree's gate, or an older tag a gate still
# uses, is left alone.
# Shared caches, kept across gates and repositories: the volumes
# synopkgland-cache-cargo (CARGO_HOME: registry and git), synopkgland-cache-npm
# (the npm cache, with the pnpm store in pnpm-store/), synopkgland-cache-gomod
# (GOMODCACHE) and synopkgland-cache-gobuild (GOCACHE). The Rust target
# directory is the volume <id>-target, kept for the next gate of the same tree
# until gate.sh --clean.
# NPM_REGISTRY: the registry npm and pnpm use, for a network that blocks
# registry.npmjs.org; the lockfiles' integrity still holds. The host's CA
# bundle, when it has one, is trusted in the container for a network that
# inspects TLS. The ui part uses the project's bounded Vitest worker count,
# leaving CPU and memory for Rust/SPK builds and the host's other gate slot.
set -euo pipefail
dir=$1 id=$2
shift 2
image=synopkgland-gate-reclaim
tag=$(sha256sum "$dir/src/scripts/gate/Dockerfile" | cut -c1-12)

if command -v flock >/dev/null; then
  waiting=0
  while :; do
    for slot in 0 1; do
      exec 9>"/tmp/synopkgland-gate.slot$slot.lock"
      if flock -n 9; then
        echo "gate slot $slot"
        break 2
      fi
      exec 9>&-
    done
    ((waiting)) || echo "waiting for a gate slot: two gates hold /tmp/synopkgland-gate.slot{0,1}.lock"
    waiting=1
    sleep 10
  done
else
  echo "no flock on this host: running without a gate slot"
fi

if ! docker image inspect "$image:$tag" >/dev/null 2>&1; then
  echo "building $image:$tag"
  docker build -q ${NPM_REGISTRY:+--build-arg "NPM_REGISTRY=$NPM_REGISTRY"} -t "$image:$tag" - <"$dir/src/scripts/gate/Dockerfile"
fi

args=(--rm --name "$id-gate" -v "$dir:/w" -v "$id-target:/w/src/target"
  -v synopkgland-cache-cargo:/cache/cargo -v synopkgland-cache-npm:/cache/npm
  -v synopkgland-cache-gomod:/cache/gomod -v synopkgland-cache-gobuild:/cache/gobuild)
ca=/etc/ssl/certs/ca-certificates.crt
if [[ -f $ca ]]; then args+=(-v "$ca:/etc/ssl/host-ca.crt:ro" -e NODE_EXTRA_CA_CERTS=/etc/ssl/host-ca.crt); fi
if [[ -n ${NPM_REGISTRY:-} ]]; then args+=(-e "npm_config_registry=$NPM_REGISTRY"); fi
[[ ${PREPARED_NODE:-0} != 1 ]] || args+=(-e SYNOPKGLAND_PREPARED_NODE=1)
# The store text catalog gate.sh copied, for check, build and inspect.
if [[ -f $dir/catalog.yaml ]]; then args+=(-e SYNOPKGLAND_CATALOG=/w/catalog.yaml); fi
if [[ -n ${GATE_JOBS:-} ]]; then args+=(-e "GATE_JOBS=$GATE_JOBS"); fi
trap 'docker rm -f "$id-gate" >/dev/null 2>&1 || true' EXIT
docker run "${args[@]}" "$image:$tag" bash /w/src/scripts/gate/inner.sh "$@"
