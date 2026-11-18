# Syno Reclaim

Syno Reclaim helps Synology NAS administrators understand what uses disk space
and how much deleting selected files or folders could actually free. It accounts
for Btrfs shared extents, snapshots and hard links rather than adding file sizes.

The DSM app and CLI provide storage analysis and explicit, reviewed cleanup.
Analysis is read-only; cleanup requires a validated plan and confirmation.
DSM app access is administrator-only, with separately approved root execution.

## Features

- Volume usage, folder exploration and reclaimable-space estimates.
- Snapshot, Recycle Bin and DSM service space attribution.
- Duplicate files and folders found from Btrfs metadata, with one-click
  selection of the copies to remove.
- Resumable scans with resource limits and saved reports.
- Cleanup into the DSM Recycle Bin, or a confirmed permanent delete where
  there is none, plus system-partition cleanup.
- DSM UI and discoverable automation tasks through `synopkgland ctl`.

## Architecture

```text
Vue UI / ctl -> SDK CGI -> Rust App -> approved SDK executor -> Rust CLI
                                                               |
                                                    Btrfs / reports / plans
```

The CGI checks DSM identity and permissions. The App runs as the package account;
the executor runs only approved root programs. The CLI owns storage accounting,
persisted jobs and cleanup validation. See the [package guide](package/README.md).

## Development

Use the Node and pnpm requirements in [ui/package.json](ui/package.json) and keep
all dependency pins and lockfiles. Native builds need rustup, Cargo,
cargo-zigbuild, Zig, Go and Python. The [gate image](scripts/gate/Dockerfile)
records the build toolchain; Docker provides the Linux build/test environment.

Install dependencies from the repository root:

```sh
npm ci
pnpm -C ui install --frozen-lockfile
```

Run the UI locally with recorded fixtures; no NAS connection is needed:

```sh
pnpm -C ui dev
```

See the [UI guide](ui/README.md) for fixture scenarios. Build and check the UI:

```sh
pnpm -C ui lint
pnpm -C ui typecheck
pnpm -C ui test
pnpm -C ui build
pnpm -C ui build:dsm
pnpm -C ui check:dsm
```

Run affected development checks through the gate:

```sh
scripts/gate.sh --fast
```

For native checks, configure `SYNOPKGLAND_SDK_DIR` as described in the package
guide. The gate runs Rust builds/tests in Docker. On Linux, the individual test
commands are `cargo test --locked` and
`cargo test --manifest-path app/Cargo.toml --locked`.

## Build an SPK

Follow the package guide to install all three Rust cross-compilation targets and
configure `SYNOPKGLAND_CATALOG` with the store-text catalog. The store text for
released versions is in the public
[synopackageland/catalog](https://github.com/synopackageland/catalog): clone it
and point `SYNOPKGLAND_CATALOG` at its `catalog.yaml`. Run these commands in a
Linux build environment with the toolchain and dependencies above:

```sh
npm run build:spk -- --json
for spk in dist/*.spk; do node_modules/.bin/synopkgland inspect "$spk" --json; done
```

The build prepares root programs, checks the manifest and produces reproducible
SPKs for x86_64, aarch64 and armv7 in `dist/`. Use `scripts/gate.sh` for the full
release gate, including packaging. Building does not install or publish a package.

## Further reading

- [CLI usage and operational limits](docs/cli-guide.md)
- [Automation tasks](docs/ctl.md)
- [System-partition cleanup](docs/rootfs-cleanup-modes.md)
- [Product and engineering specification](SPEC.md)
- [Artwork attribution](NOTICE) and [icon provenance](package/icon-source.json)

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) and [LICENSES](LICENSES) for artwork and
third-party attribution.
