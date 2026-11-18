# Dependency notices

Build 1573 uses @synopackageland/sdk 0.12.2 (Rust tag v0.12.2),
@synopackageland/ui 0.10.3 and d3-hierarchy 3.1.2.

- SDK.txt: the SDK release license, copied from tag v0.12.2.
- UI-LICENSE and UI-THIRD_PARTY.md: the license and aggregate dependency/asset
  notices shipped in the immutable @synopackageland/ui 0.10.3 npm archive.
- D3-HIERARCHY-LICENSE: the installed d3-hierarchy 3.1.2 license.

The pinned @synopackageland/cli 0.12.2 (templates 0.12.2) copies this directory,
the SDK native bridge sources/notices, and the Go, Rust and musl notices into
each SPK. The CLI and templates themselves are build tools.

Refresh these files from the pinned releases when dependencies change.
