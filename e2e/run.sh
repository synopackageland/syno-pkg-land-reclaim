#!/bin/sh
# Serves the built App through the SDK's Go runtime with the SDK's fake
# executor (delegate/delegatetest) on the runtime's executor socket. Runs as
# any user on Linux or macOS: the socket lives in a temporary directory.
#   RECLAIM_APP=<synoreclaim-app> e2e/run.sh [go test flags]
set -eu
: "${RECLAIM_APP:?the built synoreclaim-app}"
cd "$(dirname "$0")"
go vet ./...
# Exercise the same entry floor and exact App policy digest as the packaged
# CGI. Without these linker pins, SDK bridge startup failures go unnoticed.
digest=$(python3 -c 'import hashlib,pathlib; print(hashlib.sha256(pathlib.Path("../app/permissions.json").read_bytes()).hexdigest())')
sdk=$(go list -m -f '{{.Dir}}' github.com/synopackageland/syno-pkg-land-sdk)
sdk_digest=$(python3 -c 'import hashlib,pathlib,sys; print(hashlib.sha256((pathlib.Path(sys.argv[1])/"api/sdk-permissions.json").read_bytes()).hexdigest())' "$sdk")
module=github.com/synopackageland/syno-pkg-land-sdk/api
RECLAIM_APP="$RECLAIM_APP" go test -ldflags "-X $module.expectedPermissionsDigest=$digest -X $module.expectedSDKPermissionsDigest=$sdk_digest -X $module.expectedEntryMode=admin-only" -count=1 "$@" ./...
