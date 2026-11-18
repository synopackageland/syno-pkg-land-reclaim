# Release notes

## 0.1.1-1630 — Shared interface and startup update

- Refresh desktop and phone corners, focus styles, overlays and touch feedback
- Improve startup feedback and authorization messages through SDK/App 0.53.15
- Update UI to 0.25.1 and CLI to ~0.52.10; preserve the existing executor digest pins
- Retain settings, saved scan reports, protected approval/revocation and x86_64, aarch64 and armv7 support

Root program bytes are unchanged from 0.1.1-1629 on all three architectures.
The SDK still pins executor/v0.53.5. Existing approval can only carry over when
the protected runtime matches and DSM reads back the exact approved tasks.

This dependency-only release is validated by builds, per-architecture CLI inspection
and archive privacy scans. English catalog screenshots use local mock data only.
Unit tests, full gates, browser regression tests, qemu, NAS acceptance and build
reproducibility are intentionally outside this release scope.

## 0.1.1-1628 — Package Land phone entry

- Open Reclaim from Package Land with the shared SDK phone layout without enabling Application Portal
- Complete first-time system-task approval directly from the phone entry, then load Overview
- Keep the DSM desktop window and administrator-only access to all App data
- Serve minified public frontend bundles without source maps; App data requires an administrator session
- Pin CLI 0.52.5, SDK/App 0.53.3, executor/v0.53.3, UI 0.23.4 and templates 0.41.3
- Include the current Recycle Bin fixes for missing shared folders and home folders
- Retain settings and saved scan reports across upgrades

Administrator reapproval is required after upgrading from the released
0.1.1-1627 baseline: executor and root program bytes changed. Permitted operations,
parameter schemas and scheduler task templates are unchanged.

| Architecture | Root program | Released → current SHA-256 prefix |
| --- | --- | --- |
| x86_64 | cli | `0842e34a2bd0` → `d674bf0997ac` |
| x86_64 | lifecycle | `0842e34a2bd0` → `d674bf0997ac` |
| aarch64 | cli | `541b6e508b31` → `e5562ae12e6e` |
| aarch64 | lifecycle | `541b6e508b31` → `e5562ae12e6e` |
| armv7 | cli | `2d43815e0f6e` → `c1a3d47d97f3` |
| armv7 | lifecycle | `2d43815e0f6e` → `c1a3d47d97f3` |

The scoped RS822+ / DSM 7.4.1 acceptance passed from a fresh install with no
approved executor tasks: native DSM login, first approval through the English
phone entry at 390 × 844, then Overview. The desktop App entry was never opened
and Application Portal remained disabled. Anonymous requests were rejected with
`session_required`; an existing normal user received `admin_required`.

All three architectures passed check, inspect and the packaged public-UI secret
scan. Native Android APK, qemu and build reproducibility were outside this
validation scope; no release was published.
