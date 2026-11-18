# Unsigned Syno Reclaim package

Syno Reclaim is a Syno Package Land App in three parts:

- The SDK's Go runtime is the package CGI (`ui/cgi/api.cgi`). It checks the DSM
  session, the origin, the admin-only entry and `app/permissions.json`, then
  starts the App once per request as the package account, never as root.
- The App (`app/`, a Rust binary on `syno-pkg-land-app`) answers business WebAPI
  methods by running the root program `cli` through the SDK executor:
  `root.run` for answers, `root.start` and `root.jobs.*` for scans and applies.
- The executor runs `cli` (the `synoreclaim` binary, `root/<arch>/cli`) as
  root with a closed params schema: `method` is one of the WebAPI methods,
  `params` their JSON text, `job_id` the queued job to start. The CLI takes the
  user from `SYNOPKGLAND_REQUESTED_BY` and keeps its state in
  `SYNOPKGLAND_ROOT_STATE`. The same binary is also the root program
  `lifecycle` (`root/<arch>/lifecycle`), which has no run or job settings: the
  executor runs it only at package stop and upgrade (`stop`,
  `prepare-upgrade`) and at uninstall (`keep-state`, `delete-state`). The
  executor refuses a session the program it names for those steps, so they
  cannot be `cli`.

Only the executor holds root, and only after an administrator approves it with
the DSM password on the app's approval screen. The App never sees a DSM session.
There is no Synology signature or publisher signature/key service.

## Store text

The package's descriptions and release notes come only from the Syno Package
Land catalog (schema 4), not from `dsm-package.yaml`: the manifest keeps
`displayname` and leaves out `description`, `i18n.info.<language>.description`
and `metadata.description`, `long_description` and `changelog`. Set
`SYNOPKGLAND_CATALOG` to the catalog file (in the shell or the project `.env`,
see `.env.example`), or pass `--catalog <catalog.yaml>` to `synopkgland check`,
`build` and `inspect`. Add the new version's notes in all nine languages under
`metadata.changelog.releases."<version>-<build>"` of the Reclaim entry before
building; the build writes them to INFO, the SPK metadata and
`dist/release-notes.md`. Inspect each SPK with the same catalog file: a
difference fails with `CATALOG_TEXT_DRIFT`. `scripts/gate.sh` passes a copy of
the file named by `SYNOPKGLAND_CATALOG` to the package part.

## Phone entry

The Package Land mobile app discovers the packaged `app/mobile.json` and opens
the SDK's `app/entry.html` with the shared phone layout. This entry works on the
DSM origin without enabling Application Portal. The desktop launcher remains a
DSM window. Reclaim requests no mobile capabilities or intake handlers.

The registration, entry page, bundled JavaScript and icons are public static
assets. They contain no user records, settings or session credentials. The SDK
verifies the DSM session and administrator role before mounting the App or
serving API data; registering a mobile entry does not grant App access.
The App's administrator-only `app.status` returns the verified bridge identity,
package version and browser locale before executor approval. It never invokes a
root operation or reads settings.
The generated phone entry supports first-time executor approval directly on a
phone. Release validation completed the approval at 390 × 844 and loaded
Overview without opening the desktop App entry or enabling Application Portal.
Packaged public JavaScript and CSS are minified, with no source maps.

## Build

For a UI-only update, reuse trusted, inspected SPKs from an unchanged runtime
source revision without invoking Cargo or rebuilding any native binary:

```sh
pnpm -C ui install --frozen-lockfile
pnpm -C ui build:dsm
node package/repackage-ui.mjs /path/to/previous-spks <previous-source-ref>
node_modules/.bin/synopkgland check --json
for spk in dist/*.spk; do node_modules/.bin/synopkgland inspect "$spk" --json; done
```

The source ref must be the exact source of those SPKs. The script refuses
runtime, native SDK/toolchain and manifest changes other than a larger build number.
It uses the pinned CLI's SPK assembler with the original App, CGI, executor
and root programs; it verifies their hashes and both policy files after packing.
The frontend must already be built. This is binary reuse, not a new reproducible
Rust build. Use the normal build below for runtime or contract changes.

Install Rust through rustup (the rustup `cargo` first on PATH), cargo-zigbuild
and zig, the Rust targets x86_64-unknown-linux-musl,
aarch64-unknown-linux-musl and armv7-unknown-linux-gnueabihf, Node 24+ and
pnpm 10.33.2. Install UI dependencies with `pnpm -C ui install --frozen-lockfile`
and CLI dependencies with `npm ci`.

```
npm run build:spk -- --json
for spk in dist/*.spk; do node_modules/.bin/synopkgland inspect "$spk" --json; done
```

`build-root-programs.sh` cross-compiles the root program for every
architecture into `root/<arch>/cli` and `root/<arch>/lifecycle`:
Rust's bundled static musl on x86_64 and aarch64, Zig's glibc 2.17 target on armv7 (DSM's older ARM
kernel rejects musl time64 calls), no paths of the build machine and no build
id. The scan worker uses Rust's musl allocator because Zig's libc allocator
can retain hundreds of MiB during a snapshot join even when the live heap is small.
The CLI builds the App, the runtime and the UI (`pnpm build:dsm`, which
writes `ui/dist-dsm/app.module.js`, lazy chunks and `app.css`), writes `bridge.json`, the executor policy
and INFO, and assembles the SPK.

`npm run check:package` verifies the pinned CLI, prepares all three architectures'
root programs, then runs `synopkgland check --json`. Run this entry point when
checking a clean checkout: `root/` contains generated, gitignored build inputs;
a direct CLI check before preparation reports `STANDALONE_ROOT_OPS_MISSING`.
The schema 2 Rust runtime supports binary root programs at `root/<arch>/<name>`;
no shell-script substitute or native-layout migration is needed. CLI, SDK/App, UI and templates releases are pinned independently in the
package manifests and lockfiles.

`npm run build:spk` runs that prepared check, then builds all three declared
architectures in one CLI invocation with `--jobs 3`.
The Docker package gate (`scripts/gate.sh`) uses this same entry point. `--verify-reproducible always`
rebuilds and compares every SPK on every run, including with a warm cache; it
fails the build if any bytes differ. The manifest's `reproducible.clean` removes
`app/target` before that second build so the Rust App is compiled again; the CLI
also uses an empty Go build cache. Root programs are prepared before the CLI
and are inputs to both passes, so checking a fresh root compilation additionally
requires running the whole command again with `target/root-*` removed and
comparing its SPK hashes. Do not override `CARGO_TARGET_DIR` for this command:
the declared Rust clean path assumes Cargo's default `app/target`.
For a single architecture, append `--arch x86_64`; the legacy release matrix
uses `--jobs 1` because each job already selects one architecture.

The App depends on the published SDK tag pinned in `app/Cargo.toml` (`SDK_TAG_REQUIRED`).
Preserve the Cargo lockfile and npm/pnpm integrity; release builds use no local
source overrides.

`operations.json` is the App's v2 operation registry. `npm run generate:operations`
derives the Rust bridge method input and uses the pinned CLI to emit permissions,
route metadata, runtime registry, ctl descriptor, AI tools and reference docs.
`npm run check:operations` refuses drift. The package check invokes
`ctl generate --check` after preparing real root payloads, before check/build.

The SDK CGI binds the generated registry and permission digests and checks route,
spec and policy equality before serving requests. `e2e/run.sh` uses the same
entry floor and permission pins and loads the generated registry; formatting and
policy tampering remain startup failures. Never hand-edit generated permissions
or canonicalize them independently of `ctl generate`.

## Gate

`scripts/gate.sh` runs the gate on the working tree, untracked files included:

- `scripts/gate.sh --fast` while developing: only the parts the paths changed
  since `origin/main` touch (`scripts/gate/affected.sh`: `ui/` runs the UI part
  alone, the Rust sources the Rust part alone, `package/` and the package files
  the SPK build, documentation nothing, anything else everything), without the
  qemu aarch64 and armv7 tests. It prints each part it skips and why.
- `scripts/gate.sh --fast --skip-package` only when the task explicitly excludes
  SPK builds: run the affected development checks and print that packaging and
  SPK inspection were excluded. This is never a release gate.
- `scripts/gate.sh` (or `--full`) once before a release: every part, the qemu
  tests included, the Rust checks plus e2e and
  the SPK of every architecture.
- `scripts/gate.sh --clean` when the tree's work is done: removes its
  container, work folder and Rust target volume from the Docker host.

Every mode first runs `scripts/check-internal-names.sh` (also in fast CI): it
fails when a tracked or untracked file names an internal `.syno` host, a
company-internal or personal domain, a private IPv4 address other than the
common examples, a work-thread id, a `/Users/<name>` folder or a secret. Machine
names are not kept in the repository; to check them too, list them in
`SYNOPKGLAND_INTERNAL_NAMES` (comma- or space-separated) or in a file named by
`SYNOPKGLAND_INTERNAL_NAMES_FILE` (the older `SYNORECLAIM_` names still work).
Write "the test NAS" or a placeholder instead. A commit hook can run the same
rules on just the added lines and the message: `--staged` and `--message FILE`.

The UI part (`pnpm` lint, schema types, typecheck, test, build, `build:dsm`,
`check:dsm` in `ui/`) runs in the gate image on `SYNOPKGLAND_DOCKER_SSH`, beside
the other parts; without it, it runs on this machine. Vitest uses one
worker and allows 30 seconds per test for mounted App fixtures while Rust/SPK
builds and a second gate share the host. Request and cancellation assertions keep
their own clocks. The Rust part (CI's Rust jobs,
then `e2e/` as uid 1000, since the SDK runtime refuses to start the App as
root) and the package part (`build-root-programs.sh`, `synopkgland check`,
build and inspect of the three architectures) run in the gate image
(`scripts/gate/Dockerfile`) on `SYNOPKGLAND_DOCKER_SSH=<ssh host>`, or local
Docker when unset, with the SDK that `app/Cargo.toml` pins cloned from
`SYNOPKGLAND_SDK_DIR`. Rust is not built on the developer's Mac.

- The image is `synopkgland-gate-reclaim:<sha12>`, the first 12 hex digits of
  the Dockerfile's SHA-256; it is built only when that tag is missing, and the
  older tags of it are kept: the shared `synopkgland-*` images and caches are
  never removed or overwritten, so a gate never breaks another gate that is
  starting or has already built its image.
- The registry, npm and Go caches are the shared volumes
  `synopkgland-cache-cargo` (`CARGO_HOME`), `synopkgland-cache-npm`,
  `synopkgland-cache-gomod` and `synopkgland-cache-gobuild`; they stay across
  gates. The Rust target directory is the volume `<GATE_ID>-target`, one per
  tree, since another tree's sources would leave cargo's freshness checks on
  mtimes it cannot trust; `--clean` removes it.
- At most two gates run on the host at once: each waits for
  `/tmp/synopkgland-gate.slot0.lock` or `slot1.lock` and says so while it waits.
- The parts, and the Docker sections after the first checks, run at once;
  each writes its log in `dist/gate/`, and a failure prints the full log of
  each failed part and section. The SPKs land in `dist/`.
- `SYNOPKGLAND_CATALOG` names the store text catalog; the package part checks,
  builds and inspects with a copy of it and returns `release-notes.md` to `dist/`.
- `SYNOPKGLAND_DOCKER_NPM_REGISTRY` sets the registry npm and pnpm use there,
  for a network that blocks registry.npmjs.org; the lockfiles' integrity still
  holds. The host's CA bundle is trusted in the container.

For a Docker host without registry access, set `SYNOPKGLAND_NODE_ON_HOST=1`:
`gate.sh` installs locked npm/pnpm dependencies and runs UI checks on the Mac,
then sends those installed dependencies to the Rust/package container. Rust,
qemu and package assembly still run on the Docker host; the pins stay unchanged.

## Lifecycle

Stop and upgrade run `lifecycle` in its stop mode after the executor has ended the
package's root jobs: a queued scan pauses for a later resume, a queued apply is
cancelled, and pending launches go. The same step removes
`/var/packages/SynoReclaim/var/privilege`, the setuid helper and grants that
builds before the SDK executor left behind (a symlink there loses only
itself). Uninstall runs the App's own hook
(`app/bin/synoreclaim-app uninstall`, which has nothing to remove) and then
`lifecycle` in its purge mode: with the wizard's delete-state box it removes the
system partition plans in `/var/lib/synoreclaim/rootfs-plans`, which root keeps
outside the root state; the executor removes the root state itself.

Use only the authorized test procedure: send SPK bytes over SSH stdin,
compare NAS SHA-256 before installing, and record existing package state.

## End-to-end test

`e2e/` serves the built App through the SDK's Go runtime, as the package CGI
does, with the SDK's fake executor (`delegate/delegatetest`) on the runtime's
executor socket: a method's `root.run`, a scan's `root.start`, a cancel's
`root.jobs.stop`, the approval status and an executor refusal. The socket is
in a temporary directory, so the tests run as any user on Linux or macOS:

```
RECLAIM_APP=<built synoreclaim-app> e2e/run.sh -v
```


## System-partition cleanup modes

See [the mode contract](../docs/rootfs-cleanup-modes.md) for standard/deep selection, effects, estimates, approval and compatibility.
