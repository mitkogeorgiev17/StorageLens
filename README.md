<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="112" alt="" />

# StorageLens

**See what fills your disk. Free it safely.**

A storage map for macOS and Windows: every file indexed in seconds, a zoomable treemap,
and cleanup suggestions that go to the Trash, with undo.

<br />

<a href="https://github.com/mitkogeorgiev17/StorageLens/releases/latest/download/StorageLens.dmg"><img alt="Download for macOS" src="https://img.shields.io/badge/Download-macOS-111111?style=for-the-badge&logo=apple&logoColor=white" height="40" /></a>
&nbsp;
<img alt="Windows: coming soon" src="https://img.shields.io/badge/Windows-coming_soon-9a9a9a?style=for-the-badge&logo=windows&logoColor=white" height="40" />
&nbsp;
<img alt="Linux: not yet" src="https://img.shields.io/badge/Linux-not_yet-9a9a9a?style=for-the-badge&logo=linux&logoColor=white" height="40" />

<sub>macOS 14+ (Apple silicon and Intel) ·
<a href="https://github.com/mitkogeorgiev17/StorageLens/releases/latest">All files and release notes</a></sub>

</div>

---

## What it does

| | |
|---|---|
| 🗺️ **Map** | Treemap and sunburst of the whole disk, grouped by folder, type, app, age or size. |
| 📊 **Overview** | How full the disk is, what kind of files fill it, and the biggest wins. |
| 🧹 **Cleanup** | Caches, build leftovers, old downloads and more, each explained, with risk levels. |
| 🐳 **Docker** | What fills Docker's disk image (images, containers, volumes, build cache), and removing what is unused. |
| 🧬 **Duplicates** | Identical files, keeping one copy of each. |
| 📦 **Apps** | Installed apps with their real footprint; uninstall with their leftovers. |
| 🚀 **Startup** | What launches at login, and switching it off. |
| 🔎 **Search** | Find any file by name across the scan. |

**Safe by design.** Everything StorageLens removes goes to the Trash first, with a receipt
before and undo after. System folders are guarded and never offered.

**Private.** Fully offline: no accounts, no telemetry, no network access, no auto-update.

## Install

### macOS

1. Download **[StorageLens.dmg](https://github.com/mitkogeorgiev17/StorageLens/releases/latest/download/StorageLens.dmg)**.
2. Open it and drag **StorageLens** into **Applications**.
3. The first time, **right-click the app → Open → Open**. StorageLens is not signed with an
   Apple developer certificate, so a plain double-click is refused once. If macOS says it
   is damaged, run `xattr -dr com.apple.quarantine /Applications/StorageLens.app`.

On first launch StorageLens asks for three permissions and waits until they are on:

| Permission | Why |
|---|---|
| Full Disk Access | Scan every folder, including Mail, Messages and app containers. |
| App Management | Move apps in Applications to the Trash. |
| Automation: Finder | Move App Store apps, which only Finder may move. |

### Windows

Coming soon: the Windows version is written but not yet tested on a real Windows machine.

### Linux

Not yet: StorageLens has no Linux scanner so far.

## Build from source

Needs Node 22+ and Rust stable ([rustup](https://rustup.rs)).

```bash
npm install
npm run build:test                      # macOS: target/release/bundle/macos/StorageLens TEST.app
npm run tauri dev                       # run with live reload
```

Local builds are named **StorageLens TEST** (`src-tauri/tauri.test.conf.json`), with
their own identifier, data folder and macOS permissions, so they never mix with the
installed release. Only `release.yml` builds the plain **StorageLens**.

Tests and lint, as gated before every merge:

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
(cd src-tauri && cargo clippy --all-targets -- -D warnings && cargo test)
npm test && npm run typecheck
```

## Releasing

Bump `version` in `src-tauri/tauri.conf.json`, merge, then tag:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

[`release.yml`](.github/workflows/release.yml) builds the macOS `.dmg` (about 15 minutes)
and publishes it as a GitHub Release. The Windows job is held until Windows is tested.
It runs only on version tags, never on ordinary pushes.

## Under the hood

| Layer | Choice |
|---|---|
| Shell | Tauri 2 |
| Core | Rust: native scanners (`getattrlistbulk` on macOS, bulk directory queries on Windows), rules engine, safety layer |
| UI | React 19 + TypeScript + Vite, canvas treemap |
| Storage | SQLite (bundled), latest scan only |

[`PLAN.md`](./PLAN.md) has the full design; [`AGENTS.md`](./AGENTS.md) has how to work in
this repo.
