#!/bin/sh
# Reinstall an UNSIGNED SPK on a DSM test box (host argument, else DSM_TEST_HOST) with plain `synopkg install`: the package
# is released unsigned and runs as its own package user.
# After install, the UI's authorization step (DSM password) grants root.
set -eu
umask 077

usage() {
  echo "Usage: $0 <spk> <thread-id> [host]" >&2
  exit 2
}

[ "$#" -eq 2 ] || [ "$#" -eq 3 ] || usage
spk=$1
thread_id=$2
host=${3:-${DSM_TEST_HOST:-}}
[ -n "$host" ] || { echo "Pass the host or set DSM_TEST_HOST" >&2; usage; }
: "${spk:?}"
: "${thread_id:?}"
: "${host:?}"

[ -f "$spk" ] || { echo "SPK file is missing" >&2; exit 1; }
case "$thread_id" in
  ''|.|..|*[!A-Za-z0-9._-]*) echo "Invalid thread id" >&2; exit 2 ;;
esac

remote_dir="/volume1/syno-reclaim-test/$thread_id"
: "${remote_dir:?}"
spk_name=$(basename "$spk")
case "$spk_name" in
  ''|.|..|*[!A-Za-z0-9._-]*) echo "SPK filename must use letters, numbers, dot, dash, or underscore" >&2; exit 2 ;;
esac
remote_spk="$remote_dir/$spk_name"
: "${remote_spk:?}"
ssh -o BatchMode=yes "$host" "mkdir -p '$remote_dir'"
cat "$spk" | ssh -o BatchMode=yes "$host" "cat > '$remote_spk' && chmod 644 '$remote_spk'"
local_sha=$(shasum -a 256 "$spk" | awk '{print $1}')
remote_sha=$(ssh -o BatchMode=yes "$host" "sha256sum '$remote_spk' | awk '{print \$1}'")
[ "$local_sha" = "$remote_sha" ] || { echo "Remote SPK checksum mismatch" >&2; exit 1; }
printf 'SPK SHA-256: %s\n' "$local_sha"

for package in SynoReclaim SynoReclaimSpike; do
  case "$package" in
    SynoReclaim) package_paths="/var/packages/SynoReclaim" ;;
    SynoReclaimSpike) package_paths="/var/packages/SynoReclaimSpike" ;;
    *) echo "Unexpected package name" >&2; exit 2 ;;
  esac
  : "${package_paths:?}"
  if ! package_status=$(ssh -o BatchMode=yes "$host" "/usr/syno/bin/synopkg status '$package'"); then
    # synopkg status returns 255 for an absent package while printing a
    # structured non_installed status; distinguish that from SSH/DSM errors.
    case "$package_status" in
      *'"status":"non_installed"'*) package_status='status=non_installed (package absent)' ;;
      *) echo "Could not read $package status from $host" >&2; exit 1 ;;
    esac
  fi
  printf 'Before install: %s: %s\n' "$package" "$package_status"
  case "$package_status" in
    *non_installed*)
      if residue=$(ssh -o BatchMode=yes "$host" "for p in $package_paths; do [ ! -e \"\$p\" ] || echo \"\$p\"; done"); then
        if [ -n "$residue" ]; then
          printf 'Pre-existing residue for %s: %s\n' "$package" "$residue"
        else
          printf 'Pre-existing residue for %s: none\n' "$package"
        fi
      else
        echo "Could not inspect residue for $package" >&2
        exit 1
      fi
      ;;
    *)
      ssh -o BatchMode=yes "$host" "/usr/syno/bin/synopkg stop '$package' >/dev/null 2>&1 || true; /usr/syno/bin/synopkg uninstall '$package' >/dev/null"
      if residue=$(ssh -o BatchMode=yes "$host" "for p in $package_paths; do [ ! -e \"\$p\" ] || echo \"\$p\"; done"); then
        if [ -n "$residue" ]; then
          printf 'Post-uninstall residue for %s: %s\n' "$package" "$residue"
        else
          printf 'Post-uninstall residue for %s: none\n' "$package"
        fi
      else
        echo "Could not inspect post-uninstall residue for $package" >&2
        exit 1
      fi
      ;;
  esac
done

if ! ssh -o BatchMode=yes "$host" "/usr/syno/bin/synopkg install '$remote_spk' >/dev/null && /usr/syno/bin/synopkg start SynoReclaim >/dev/null"; then
  echo "$host package install or start failed" >&2
  exit 1
fi
if ! installed_status=$(ssh -o BatchMode=yes "$host" '/usr/syno/bin/synopkg status SynoReclaim'); then
  echo "Could not read installed SynoReclaim status" >&2
  exit 1
fi
printf 'Installed status: %s\n' "$installed_status"
if ! installed_info=$(ssh -o BatchMode=yes "$host" "sed -n '/^version=/p' /var/packages/SynoReclaim/INFO"); then
  echo "Could not read installed SynoReclaim package version" >&2
  exit 1
fi
[ -n "$installed_info" ] || { echo "Installed SynoReclaim package version is missing" >&2; exit 1; }
printf 'Installed package version: %s\n' "$installed_info"
