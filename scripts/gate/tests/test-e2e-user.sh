#!/usr/bin/env bash
# Run inside the gate image as root. Exercise the real launcher with a private
# mktemp parent, as gate.sh's archive creates, without recompiling the App.
set -euo pipefail
launcher=$(cd "$(dirname "$0")/.." && pwd)/e2e.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src/e2e" "$work/src/target/app/release" "$work/sdk.git" "$work/bin"
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $* == 'build --manifest-path app/Cargo.toml --locked --release' ]]
[[ $CARGO_TARGET_DIR == "$PWD/target/app" ]]
EOF
cat >"$work/src/target/app/release/synoreclaim-app" <<'EOF'
#!/usr/bin/env bash
[[ $(id -u) == 1000 && $(id -g) == 1000 ]]
EOF
cat >"$work/src/e2e/run.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cd "$PWD/$(dirname "$0")"
[[ $(id -u) == 1000 && $(id -g) == 1000 ]]
[[ $(id -G) == 1000 ]]
[[ $* == -v ]]
[[ -r ../../sdk.git/HEAD ]]
"$RECLAIM_APP"
echo 'non-root E2E launcher: pass'
EOF
echo 'ref: refs/heads/main' >"$work/sdk.git/HEAD"
chmod +x "$work/bin/cargo" "$work/src/e2e/run.sh" "$work/src/target/app/release/synoreclaim-app"
# Another gate job's build output: the launcher must leave target/ alone but
# for the release App's path, so it cannot race files that job creates and
# removes while the launcher runs.
mkdir -p "$work/src/target/root-x86_64/deps"
touch "$work/src/target/root-x86_64/deps/rustcTEMP"
chmod 700 "$work/src/target/root-x86_64"
chmod 600 "$work/src/target/root-x86_64/deps/rustcTEMP"
chmod 700 "$work/src/target" "$work/src/target/app" "$work/src/target/app/release"
# The checks job's test scratch (TMPDIR=.tmp) with a symlink whose target is
# gone, and a dangling symlink in the source: neither may fail the launcher.
mkdir -p "$work/src/.tmp/synoreclaim-test/proc"
touch "$work/src/.tmp/synoreclaim-test/state"
chmod 600 "$work/src/.tmp/synoreclaim-test/state"
ln -s /proc/999999999 "$work/src/.tmp/synoreclaim-test/proc/999999999"
ln -s missing "$work/src/e2e/dangling"
[[ $(stat -c %a "$work") == 700 ]]
PATH="$work/bin:$PATH" bash "$launcher" "$work"
# Traversal suffices; the launcher must not make the parent's listings public
# or make source/SDK files writable by the package user.
[[ $(stat -c %a "$work") == 711 ]]
[[ $(stat -c %a "$work/src/e2e/run.sh") == 755 ]]
[[ $(stat -c %a "$work/sdk.git/HEAD") == 644 ]]
[[ $(stat -c %a "$work/src/target/root-x86_64") == 700 ]]
[[ $(stat -c %a "$work/src/target/root-x86_64/deps/rustcTEMP") == 600 ]]
[[ $(stat -c %a "$work/src/target/app/release/synoreclaim-app") == 755 ]]
[[ $(stat -c %a "$work/src/.tmp/synoreclaim-test/state") == 600 ]]
