#!/usr/bin/env bash
# Builds the synoreclaim binary for every DSM architecture into the executor's
# root programs root/<arch>/cli, for sessions, and root/<arch>/lifecycle, for
# the package stop and uninstall purge steps, where synopkgland build packs them.
# The executor refuses a session the program it runs for those steps.
# Use Rust's bundled musl for the scan worker: Zig's libc allocator can retain
# hundreds of MiB during a snapshot join despite a small live heap. armv7
# still needs Zig's glibc 2.17 target for the older DSM kernel.
# No paths of this machine or build id, so a rebuild is identical.
# The architectures build at once, each in its own target directory
# (target/root-<arch>) so that cargo's lock on one does not queue the others;
# each writes target/root-<arch>.log, printed in full when its build fails.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo_home=${CARGO_HOME:-$HOME/.cargo}
flags=(
  "--remap-path-prefix=$PWD=synoreclaim"
  "--remap-path-prefix=$cargo_home=cargo"
  -Cstrip=symbols
  -Alinker_messages
)
export SOURCE_DATE_EPOCH=0
unset RUSTFLAGS
build_root() {
  local arch=$1 triple=$2
  local build_flags=("${flags[@]}")
  export CARGO_TARGET_DIR=target/root-$arch
  if [[ $triple == *-musl ]]; then
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
    build_flags+=(-Clink-arg=--build-id=none)
    CARGO_ENCODED_RUSTFLAGS=$(IFS=$'\x1f'; echo "${build_flags[*]}")
    export CARGO_ENCODED_RUSTFLAGS
    cargo build --release --locked --target "$triple" --bin synoreclaim
  else
    build_flags+=(-Clink-arg=-Wl,--build-id=none)
    CARGO_ENCODED_RUSTFLAGS=$(IFS=$'\x1f'; echo "${build_flags[*]}")
    export CARGO_ENCODED_RUSTFLAGS
    cargo zigbuild --release --locked --target "$triple" --bin synoreclaim
  fi
}
pairs=(x86_64:x86_64-unknown-linux-musl aarch64:aarch64-unknown-linux-musl armv7:armv7-unknown-linux-gnueabihf.2.17)
mkdir -p target
pids=()
for pair in "${pairs[@]}"; do
  arch=${pair%%:*} triple=${pair#*:}
  build_root "$arch" "$triple" >"target/root-$arch.log" 2>&1 &
  pids+=($!)
done
failed=0
for i in "${!pairs[@]}"; do
  arch=${pairs[i]%%:*} triple=${pairs[i]#*:}
  if ! wait "${pids[i]}"; then
    echo "root program $arch failed; target/root-$arch.log:" >&2
    cat "target/root-$arch.log" >&2
    failed=1
    continue
  fi
  mkdir -p "root/$arch"
  for program in cli lifecycle; do
    install -m 0755 "target/root-$arch/${triple%.2.17}/release/synoreclaim" "root/$arch/$program"
  done
done
exit "$failed"
