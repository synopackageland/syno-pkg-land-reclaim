#!/usr/bin/env bash
# Usage: inner.sh <fast|full> <part>...   (parts: ui rust package)
# Runs in the gate image (scripts/gate/Dockerfile) that scripts/gate.sh starts:
# the tree in /w/src, a bare SDK clone in /w/sdk.git, each section's log in
# /w/logs/<section>.log. The rust part runs the Rust format, lint and test
# jobs and e2e/; the qemu aarch64 and armv7 tests run only in the full gate.
# The package part builds and inspects the SPK of every architecture. The ui
# part runs ui/'s checks; it and the package part share ui/node_modules, which
# ui_deps installs once, beside the first checks.
# Sections after the first checks run in parallel, GATE_JOBS (default 4) at once.
set -euo pipefail
mode=$1; shift
parts=" $* "
logs=/w/logs
mkdir -p "$logs"
cd /w/src

export CARGO_HOME=/cache/cargo CARGO_NET_GIT_FETCH_WITH_CLI=true RUST_BACKTRACE=1
export npm_config_cache=/cache/npm npm_config_store_dir=/cache/npm/pnpm-store
export GOMODCACHE=/cache/gomod GOCACHE=/cache/gobuild GOFLAGS=-buildvcs=false
export GOPRIVATE='github.com/synopackageland/*'
git config --global url.file:///w/sdk.git.insteadOf https://github.com/synopackageland/syno-pkg-land-sdk
git config --global --add safe.directory '*'

# The caches are shared with other gates, some running as uid 1000, and /w
# belongs to whoever started the gate: leave both writable for them.
release() {
  find /cache -user 0 -exec chmod a+rwX {} + 2>/dev/null || true
  chown -R "$(stat -c %u:%g /w)" /w/logs /w/src 2>/dev/null || true
}
trap release EXIT
chown 1000:1000 /cache/gomod /cache/gobuild

# section <name> <function>: runs the function with its output in its log and
# records "<name> <exit code> <seconds>" in the status file. The function runs
# in a new bash: called from section's || context, set -e would not stop it.
# The sections other than ui's run at nice 10: the ui tests' timeouts need
# their share of the CPUs the Rust builds would take.
section() {
  local name=$1 start=$SECONDS rc=0 niceness=10
  [[ $name == ui* ]] && niceness=0
  nice -n "$niceness" bash -euo pipefail -c "$(declare -f); $2" >"$logs/$name.log" 2>&1 || rc=$?
  echo "$name $rc $((SECONDS - start))" >>"$logs/status"
  if ((rc)); then echo "FAIL $name ($((SECONDS - start)) s)"; else echo "ok   $name ($((SECONDS - start)) s)"; fi
  return "$rc"
}

checks() {
  bash scripts/gate/tests/test-e2e-user.sh
  cargo fetch --locked
  cargo fetch --manifest-path app/Cargo.toml --locked
  cargo fmt --check
  cargo fmt --manifest-path app/Cargo.toml -- --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo clippy --manifest-path app/Cargo.toml --locked --all-targets -- -D warnings
  mkdir -p .tmp
  rustc --edition 2024 --test -D warnings build.rs -o .tmp/build-script-tests
  .tmp/build-script-tests
  # Every test binary and suite runs even when an earlier one fails, so one
  # failing unit test does not hide the integration, App and ctl results;
  # the section still fails at the end if any of them failed.
  local tests_failed=0
  TMPDIR=$PWD/.tmp cargo test --locked --no-fail-fast || tests_failed=1
  RECLAIM_CTL_RESULTS=$PWD/.tmp/ctl-results.json TMPDIR=$PWD/.tmp CARGO_TARGET_DIR=$PWD/target/app cargo test --manifest-path app/Cargo.toml --locked --no-fail-fast || tests_failed=1
  if [[ -d tests/ctl ]]; then
    [[ ${SYNOPKGLAND_PREPARED_NODE:-0} == 1 ]] || npm ci --ignore-scripts --no-audit --no-fund
    local sdk_ref
    sdk_ref=$(python3 -c 'import tomllib; dep=tomllib.load(open("app/Cargo.toml", "rb"))["dependencies"]["syno-pkg-land-app"]; print(dep.get("rev") or dep["tag"])')
    git clone --quiet /w/sdk.git /w/ctl-sdk
    git -C /w/ctl-sdk checkout --quiet "$sdk_ref"
    node package/generate-operations.mjs --check
    SYNOPKGLAND_SDK_DIR=/w/ctl-sdk node --test tests/ctl/*.test.mjs || tests_failed=1
  fi
  return "$tests_failed"
}

test_aarch64() {
  mkdir -p .tmp/aarch64
  CARGO_TARGET_DIR=$PWD/target/test-aarch64 \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUNNER=qemu-aarch64-static \
    TMPDIR=$PWD/.tmp/aarch64 cargo test --locked --no-fail-fast --target aarch64-unknown-linux-musl
}

test_armv7() {
  mkdir -p .tmp/armv7
  # No QEMU_LD_PREFIX: qemu-user applies that prefix to every absolute path
  # the tests open, so File::open("/") opened /usr/arm-linux-gnueabihf and
  # every walk from / (cleanup recover and apply, the rootfs walks) failed
  # with ENOENT. The armhf loader goes where the binaries name it instead,
  # and only the guest gets the armhf library path. The tests still skipped
  # fail only under qemu-user: the first two always, the last when its setup
  # takes longer than the job progress interval under emulation.
  ln -sf /usr/arm-linux-gnueabihf/lib/ld-linux-armhf.so.3 /lib/ld-linux-armhf.so.3
  CARGO_TARGET_DIR=$PWD/target/test-armv7 \
    CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc \
    CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER="qemu-arm-static -E LD_LIBRARY_PATH=/usr/arm-linux-gnueabihf/lib" \
    TMPDIR=$PWD/.tmp/armv7 cargo test --locked --no-fail-fast --target armv7-unknown-linux-gnueabihf -- \
    --skip rootfs::delegate::tests::a_run_reports_its_exit_with_a_cleared_environment \
    --skip rootfs::apply::tests::a_copy_that_fails_midway_is_removed \
    --skip commands::tests::a_cleanup_job_starts_with_its_totals_and_follows_the_log
}

# build_musl <triple> <file pattern>
build_musl() {
  local dir=$PWD/target/musl-${1%%-*}
  CARGO_TARGET_DIR=$dir CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    cargo build --locked --release --target "$1"
  file "$dir/$1/release/synoreclaim" | grep -E "$2"
}
build_musl_x86_64() { build_musl x86_64-unknown-linux-musl 'ELF 64-bit.*x86-64.*(statically|static-pie) linked'; }
build_musl_aarch64() { build_musl aarch64-unknown-linux-musl 'ELF 64-bit.*ARM aarch64.*(statically|static-pie) linked'; }

# Not root: the SDK runtime refuses to start the App as root.
e2e() { bash scripts/gate/e2e.sh /w; }

ui_deps() {
  [[ ${SYNOPKGLAND_PREPARED_NODE:-0} == 1 ]] || pnpm -C ui install --frozen-lockfile
}

# The generated schema types must be the committed ones.
ui() {
  cp -R ui/src/types/schema /tmp/committed-schema
  pnpm -C ui lint
  pnpm -C ui gen:types
  diff -r /tmp/committed-schema ui/src/types/schema
  pnpm -C ui typecheck
  pnpm -C ui test
  pnpm -C ui build
  pnpm -C ui build:dsm
  pnpm -C ui check:dsm
}

package() {
  [[ ${SYNOPKGLAND_PREPARED_NODE:-0} == 1 ]] || npm ci --no-audit --no-fund
  python3 -m unittest package/tests/test-verify-cli.py
  python3 -m unittest package/tests/test-build-spk.py
  rm -rf dist
  npm run build:spk -- --json
  for spk in dist/*.spk; do node_modules/.bin/synopkgland inspect "$spk" --json; done
  sha256sum dist/*.spk
}

failed=0 deps=
if [[ $parts == *" ui "* || $parts == *" package "* ]]; then
  section ui_deps ui_deps &
  deps=$!
fi
if [[ $parts == *" rust "* ]]; then
  section checks checks || failed=1
fi
if [[ -n $deps ]]; then wait "$deps" || failed=1; fi

queue=()
[[ $parts == *" ui "* ]] && queue+=(ui)
[[ $parts == *" package "* ]] && queue+=(package)
if [[ $parts == *" rust "* ]]; then
  if [[ $mode == full ]]; then
    queue+=(test_aarch64 test_armv7)
  else
    echo "skip test_aarch64 test_armv7: the fast gate leaves the qemu tests to the full gate"
  fi
  queue+=(build_musl_x86_64 build_musl_aarch64 e2e)
fi

if ((failed)); then
  echo "skip ${queue[*]-}: checks or ui_deps failed"
else
  for job in "${queue[@]+"${queue[@]}"}"; do
    while (($(jobs -rp | wc -l) >= ${GATE_JOBS:-4})); do wait -n || true; done
    section "$job" "$job" &
  done
  while (($(jobs -rp | wc -l))); do wait -n || true; done
fi

[[ -f $logs/status ]] || exit 0
! awk '$2 != 0 { bad = 1 } END { exit !bad }' "$logs/status"
