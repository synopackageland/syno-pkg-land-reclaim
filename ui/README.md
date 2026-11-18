# Syno Reclaim UI

The Vue 3.5 + TypeScript + Vite app of the Syno Reclaim DSM package. CLI behaviour
is in [SPEC.md](../SPEC.md).

## Run

Needs Node 24 and pnpm (`corepack enable`; the version is pinned in
`package.json`). Dependencies come from public npmjs (`.npmrc`; no token or
private CA needed). Components come from the pinned public
`@synopackageland/ui` package; its exact version is recorded in the manifest
and lockfile.

```bash
pnpm -C ui install
pnpm -C ui dev          # http://localhost:5173, served from ui/fixtures/
pnpm -C ui lint
pnpm -C ui typecheck
pnpm -C ui test
pnpm -C ui build        # ui/dist/
pnpm -C ui gen:types    # after a schemas/v1 change; commit src/types/schema/
```

Outside DSM the dev server answers `api()` from `ui/fixtures/`. Pick a state
with `?scenario=<name>` (`normal`, `no_report`, `scanning`, `paused`, `partial`,
`busy`, `error`, `not_admin`) or the select in the toolbar. The language follows
the browser; to see zh-TW, set the browser language to zh-TW, or define
`window._S = k => k === 'lang' ? 'cht' : undefined` before the app loads (what
DSM provides).

## Layout

```
ui/
  build/                 build-time helpers (theme :root re-scoping, type generation)
  fixtures/              recorded CLI JSON for dev and tests (see fixtures/README.md)
  src/
    api/                 api(method, params): DSM WebAPI or fixtures; typed methods, ApiError
    types/               documents generated from schemas/v1 (schema/) + short names (index.ts)
    i18n/                t(), code(), formatters; messages in <lang>/<name>.json
    stores/              session, volumes, jobs (polling), basket, nav
    components/states/   StateView + blockingState(): the global states of SPEC §3
    shell/               AppShell (sidebar, toolbar, job strip), dev scenario select
    pages/               one folder per page; registry.ts lists them all
```

## Rootfs cleanup dialog preview

`src/rootfs/RootfsCleanupDialog.vue` is mounted by the System partition view
and exposes `open(): Promise<void>` for its Clean up entry point. The confirmed
review step starts `rootfs.apply`; the dialog then hosts the progress,
result and cleanup history view.

The dev-only fixture host is `http://localhost:5173/rootfs-cleanup.html`. Add
`?open=1&scenario=rootfs_cleanup` to open the dialog on load. The preview also
supports `rootfs_no_room`, `rootfs_no_shares`, `rootfs_destination_full`,
`rootfs_busy`, `rootfs_walk_stopped` and other rootfs plan scenarios.

## Page conventions

- **Your files.** Work only in `src/pages/<Page>/` and your i18n files
  `src/i18n/<name>/en.json` + `src/i18n/<name>/zh-TW.json`. The registry
  (`src/pages/registry.ts`) already lists every page; the shell never needs
  editing. Page-local components go in your page folder; move a component to
  `src/components/` only when a second page needs it.
- **Pages.** `OverviewPage.vue`, `ExplorePage.vue`, `SnapshotsPage.vue`,
  `RecyclePage.vue`, `HistoryPage.vue`, `ScanningPage.vue` (+ `JobStrip.vue`),
  `SettingsPage.vue`, and the dialogs `NewScan/NewScanDialog.vue` and
  `Review/ReviewDialog.vue` (both `v-model:open`). Switch pages with
  `navigate('<key>')` and open the scan dialog with `openNewScan()`
  (`@/stores/nav`).
- **Toolbar chip.** Put the page's data-time chip in the page header with
  the SDK's `<PageActions>` (`@synopackageland/sdk/vue`); a test of one page
  shows it in place with `showPageActionsInPlace()` (`@/shell/pageActionsInPlace`).
- **Data.** Only through `api(method, params)` from `@/api`; types come from
  `@/types`. Never parse CLI text; read `completion`, `errors[].code`,
  `warnings[].code`. Shared state: `session`, `volumes`/`selectedVolume`,
  `jobs`/`runningJobs`/`pollNow()`, `useBasket(volume)`. Keep additions to
  shared code minimal.
- **Components.** Use `@synopackageland/ui` first (run
  `npx --no syno-pkg-land-ui catalog --json` for its components). Overlays (Dialog, Sheet, Popover, Toast) already
  teleport into the app root. Charts are hand-written SVG; no chart library.
- **States.** Use `<StateView kind="no_report" | "partial" | "stale" | "busy" | "error" …>`
  from `@/components/states/StateView.vue` for the SPEC §3 states.
- **i18n.** Every visible string goes through `t()`; CLI codes through
  `code(set, value)` (keys `code.<set>.<value>` in `codes.json`; unknown codes
  show "Unrecognised status (…)"). A file `<name>.json` holds only keys that
  start with `<name>.` (`codes.json`: `code.`), and zh-TW must have exactly the
  English keys: tests enforce both. Plurals: `{ "one": …, "other": … }`
  messages, chosen by the `count` param through the SDK's `format`. Numbers, bytes, dates, durations only via the formatters in
  `@/i18n` (`null` shows "unknown", never 0).
- **Tests.** `*.test.ts` next to the code (Vitest + @vue/test-utils, jsdom).
  Point `api()` at fixtures with
  `setBackend(fixtureBackend({ scenario }))` (`@/api`, `@/api/fixtures`). See
  `src/App.test.ts` for mounting with async pages.
- **Fixtures.** Add recorded documents under `ui/fixtures/<method>/` with an
  `index.json` entry (fixtures/README.md). The contract test validates each one
  against `schemas/v1`. Do not run new NAS scans for UI work.
