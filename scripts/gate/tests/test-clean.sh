#!/usr/bin/env bash
# Tests gate.sh --clean against a fake docker: it waits for a container that
# is still being removed before it removes the target volume, and it reports
# what it could not remove instead of claiming it removed it.
set -euo pipefail
gate=$(cd "$(dirname "$0")/../.." && pwd)/gate.sh
failures=0
fail() { echo "FAIL: $*"; failures=$((failures + 1)); }

# fake <state dir>: a docker that knows one container and one volume. The
# container answers inspect <state>/removing more times after rm -f, as a --rm
# container docker is still removing does; the volume stays in use while the
# container exists, or for good when <state>/held exists.
fake() {
  cat >"$1/docker" <<'EOF'
#!/usr/bin/env bash
state=$(dirname "$0")
case "$1 $2" in
  'rm -f') [[ -f $state/container ]] || exit 1; rm -f "$state/container"; [[ -s $state/removing ]] && exit 1; exit 0 ;;
  'container inspect')
    n=$(cat "$state/removing" 2>/dev/null || echo 0)
    if [[ -f $state/container ]] || ((n > 0)); then echo $((n - 1)) >"$state/removing"; exit 0; fi
    exit 1 ;;
  'volume inspect') [[ -f $state/volume ]] ;;
  'volume rm')
    [[ -f $state/volume ]] || { echo "no such volume" >&2; exit 1; }
    n=$(cat "$state/removing" 2>/dev/null || echo 0)
    if [[ -f $state/held ]] || ((n > 0)); then echo "volume is in use" >&2; exit 1; fi
    rm -f "$state/volume" ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$1/docker"
}

clean() {
  local state=$1
  PATH="$state:$PATH" GATE_ID=t-test-clean-$$ SYNOPKGLAND_DOCKER_SSH='' "$gate" --clean
}

state=$(mktemp -d)
fake "$state"
touch "$state/container" "$state/volume"
echo 3 >"$state/removing"
if out=$(clean "$state" 2>&1); then
  [[ ! -f $state/volume ]] || fail "the volume of a container still being removed was kept: $out"
  [[ $out == *"removed t-test-clean-$$-gate, t-test-clean-$$-target"* ]] || fail "no removal reported: $out"
else
  fail "clean failed while the container was still being removed: $out"
fi
rm -rf "$state"

state=$(mktemp -d)
fake "$state"
touch "$state/volume" "$state/held"
if out=$(clean "$state" 2>&1); then
  fail "clean succeeded with the volume still in use: $out"
else
  [[ $out == *"t-test-clean-$$-target"*"still there"* || $out == *"still there"*"t-test-clean-$$-target"* ]] || fail "the kept volume is not named: $out"
  [[ $out != *"removed t-test-clean-$$-gate, t-test-clean-$$-target"* ]] || fail "a kept volume was reported removed: $out"
fi
rm -rf "$state"

if ((failures)); then exit 1; fi
echo "gate.sh --clean: all cases pass"
