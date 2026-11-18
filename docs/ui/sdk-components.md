# SDK component inventory (`@synopackageland/ui`)

Rule: use the SDK component wherever one exists; keep our own only where the SDK has nothing equivalent, and build those from SDK primitives and tokens.

Upgrade 0.4.2 → 0.5.3 needed no code changes. New in this range and now used: `SettingsSection`, `SettingsRow`, `Breadcrumb`, `RadioGroup`, `ExpandableItem`, `Pagination`, `TypoA`, `Progress`.

## Replaced

| Was | Now (SDK) |
| --- | --- |
| `SettingsGroup`, `SettingsControlRow`, `settingsLayout` | `SettingsSection` (non-collapsible), `SettingsRow` |
| `Breadcrumb.vue` (Explore) | `Breadcrumb` |
| New Scan volume / scope / mode / resume cards | `RadioGroup` |
| Scanning page resume-mode buttons | `RadioGroup` |
| `OptionRow`, hand-rolled picker groups and pager | `Checkbox`, `RadioGroup`, `ExpandableItem`, `Pagination` (`OptionRows`, `OptionPicker`) |
| Rootfs cleanup dialog notices, no-undo warning, `<details>` | `Callout`, `ExpandableItem` |
| Review / cleanup `<progress>` | `Progress` |
| Plain KB `<a>` links (Overview, RootfsApply) | `TypoA` |

## Mapping of what remains

| Component / page | SDK equivalent | Decision |
| --- | --- | --- |
| `App.vue`, `shell/AppShell.vue` | `AppRoot`, `AppLayout`, `AppSidebar`, `AppToolbar`, `ToastProvider` | SDK |
| `shell/DevScenarioSelect.vue` | `Select` | SDK (dev only) |
| `pages/RootfsApply/RootfsApplyFixtureApp.vue` | none | keep: dev fixture host with a native `<select>` |
| `BasketBar` | `AppActionBar` | SDK |
| `ConfirmDialog` | `Dialog`, `Checkbox` | SDK |
| `JobStrip` | `Surface`, `Progress`, `Dialog` | SDK; layout wrapper only |
| `CancelButton` | `Button` | keep: SDK `Button` with an outlined-danger tint (the SDK has no such variant) |
| `ProgressRing` | none (`Progress` is linear) | keep: SVG ring for the scan page |
| `Donut`, `LineChart`, `Treemap` | none | keep: charts; SVG from tokens |
| `PhaseList` | none | keep: scan phase checklist |
| `ReclaimGrid`, History grids | `DataGrid` | SDK |
| `VolumeCard`, `VolumeCapabilities`, `VolumePicker` | `Badge`, `Tooltip`, `Callout`, `Select`, `SectionTabs` | SDK |
| `SystemAreasNote`, `NamespaceChurnNote`, `UnattributedNote`, `StatusNote`, `UnsupportedVolume*` | `Callout`, `Tooltip` | SDK |
| `states/StateView`, `AuthorizationView` | `EmptyState`, `Callout`, `Button` | SDK |
| `OptionPicker`, `OptionRows` | `SearchField`, `ExpandableItem`, `Checkbox`, `RadioGroup`, `Pagination` | SDK |
| Overview (`OverviewPage`, `SpacePanel`, `ActionList`, `WatchFreeSheet`, `SystemPartitionView`) | `Surface`, `Tabs`, `Sheet`, `Accordion`, `Badge`, `Callout`, `TypoA` | SDK |
| Explore page | `Breadcrumb`, `SectionTabs`, `Sheet`, `SearchField`, `Checkbox` | SDK |
| Snapshots, Recycle Bin, Packages, History | `Surface`, `Select`, `DataGrid`, `EmptyState`, `Dialog` | SDK |
| Scanning page | `Callout`, `Dialog`, `RadioGroup`, `Surface` | SDK, plus the kept `ProgressRing` and `PhaseList` |
| New Scan dialog | `Dialog`, `RadioGroup`, `Checkbox`, `Callout` | SDK |
| Review dialog, `CleanupApplyPage` | `Progress`, `Callout` | SDK |
| `rootfs/RootfsCleanupDialog` | `Dialog`, `Callout`, `ExpandableItem`, `Checkbox` | SDK; the step dots and stat tiles are kept (no stepper or stat component) |
| `RootfsApplyStep`, `RootfsHistorySection` | `Progress`, `Surface`, `Dialog`, `TypoA` | SDK |
| Settings page | `SettingsSection`, `SettingsRow`, `Switch` | SDK |
| `usage/UsageChangeBanner` | `Callout` | SDK |
