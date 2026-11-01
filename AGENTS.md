# AGENTS.md

Instructions for AI coding agents working in this repository.

**Read [`PLAN.md`](./PLAN.md) before writing any code.** It is the source of truth for
architecture, data model, and design decisions. This file covers *how to work here*;
`PLAN.md` covers *what we are building and why*.

---

## 1. What this is

A personal storage mapper for **macOS and Windows**. It indexes every file on the
machine, visualizes it as a zoomable treemap, groups it by type / app / age /
cleanliness, and recommends safely deletable junk — with Trash + undo.

- **Audience:** one person. It is not a product. No users, no support, no growth.
- **Tauri 2** shell, **Rust** core (both scanners), **React + TypeScript** UI.
- **Fully offline.** No network code exists in this repo, and none may be added.

Optimize for **correctness and simplicity**. Not for scale, not for configurability,
not for hypothetical other users.

### Current state

**v1 is feature-complete** (2026-11-01): M0–M6 are merged. Both scanners, the
catalog, the Overview, Map, Large Files, Apps, Startup and Cleanup (with duplicates)
screens, the rules engine, and the safety layer (guards, Trash, undo) exist and pass
the local suite on macOS. There is no History screen (dropped); undo is offered
on the result panel right after each move.

**Work is tracked as GitHub issues, not as tasks in this file.** One issue is open:

- **#16 — USN/MFT fast path** (Windows, stretch). Parked on purpose: PLAN §7.2 says
  build it only if the v1 Windows scan feels slow, and that has not been measured.

**Known gap — Windows has only been type-checked.** The Windows scanner, app
inventory (`scan-windows/src/apps.rs`), and the app's command tests have never run
on a Windows machine (§4). Before trusting Windows numbers, run the suite there,
including `scanbench/tests/accuracy_windows.rs`. Do not describe Windows behaviour
as tested until that happens.

| Milestone | Issues | Delivered |
|---|---|---|
| M0 — Foundation | #1–#6 | Workspace, domain model, schema, fixtures, `scanbench` |
| M1 — macOS scanner | #7–#11 | `getattrlistbulk`, dedupe, FDA, 2% accuracy gate |
| M2 — Windows scanner | #12–#15 | Bulk directory query, sizes, registry, accuracy gate (#16 parked) |
| M3 — Catalog & treemap | #17–#24 | Arena, IPC, React shell, canvas treemap |
| M3b — Categorization & grouping | #25–#28 | Six groupings, filters, sunburst (collections dropped) |
| M4 — Cleanup & safety | #29–#35 | Guards, trash + undo, rules, catalogs, UI |
| M5 — App inventory | #36–#38 | Installed apps, usage, footprint, removal |
| M6 — Duplicates | #39–#40 | Hash pipeline, keep-one UI |

New work starts as a new issue. Follow the dependency links and `Touches` lists (§9).

See §9 for the branch and PR workflow.

---

## 2. Ground rules

1. **`PLAN.md` is authoritative.** If you believe a design decision is wrong, say so and
   propose an edit to `PLAN.md`. Do not silently diverge in code.
2. **No network.** No HTTP clients, no telemetry, no analytics, no crash uploaders, no
   auto-update, no "phone home" of any kind. A dependency that opens sockets is a bug.
3. **No new dependencies without justification.** State what it replaces and why std /
   existing crates are insufficient. Prefer std.
4. **One platform at a time.** Do not try to make the macOS and Windows scanners
   identical. They share a trait, not an implementation.
5. **Ask before:** changing the SQLite schema, adding a dependency, or altering a
   decision recorded in `PLAN.md`.

---

## 3. Repository map

```
crates/core/           Entry, Volume, ScanOptions, StorageScanner trait, errors
  categories.rs        ~400-entry shared extension → category table
  tags.rs              u64 tag bitmask
crates/catalog/        SQLite schema, migrations, queries, tree arena, rollups
crates/scan-macos/     getattrlistbulk FFI, buffer parser, app inventory (apps.rs)
crates/scan-windows/   bulk dir-query walk, registry, app inventory (apps.rs)
crates/analysis/       rules engine, duplicate finder (dupes.rs), app classification (apps.rs)
  grouping.rs          synthetic tree builders (Folder/Type/App/Age/Size/Cleanliness)
crates/safety/         path guards, trash service, manifests, undo
crates/scanbench/      CLI binary — no UI, used to validate the scanners
src-tauri/             Tauri app, #[tauri::command] surface, event emitters
ui/                    React + TypeScript frontend
package.json           root npm workspace: Tauri CLI + scripts (owns ui/)
```

**Dependency direction is one-way:** `scan-*` → `core` ← `analysis` → `safety`.
`core` must never depend on a platform crate, and `analysis` must never call OS APIs
directly.

---

## 4. Build and test

Both Cargo workspaces (root and `src-tauri`) build into one shared `target/` folder
(`.cargo/config.toml`, #177), so shared crates compile once; the app bundle lands in
`target/release/bundle/`. Dev builds keep line tables only. `cargo clean` frees it all.

One-time setup (Node 22+, Rust stable via `rustup`):

```bash
npm install          # root workspace — installs the Tauri CLI and ui/
```

```bash
# Rust workspace (7 crates, no frontend needed)
cargo build --workspace
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check

# Fixture tree generator only (off by default; never ships in the app)
cargo test -p storagelens-core --features fixtures

# Scanner validation (no UI) — the important one
cargo run -p storagelens-scanbench --release -- ~/Documents
cargo run -p storagelens-scanbench --release -- --fixtures   # exact counts + bytes
cargo run -p storagelens-scanbench --release -- ~/Documents --budget  # exit 1 on a miss

# UI only
npm run typecheck
npm run build

# App (dev) — starts Vite, then the Tauri window
npm run tauri dev

# Release build
npm run tauri build
```

`src-tauri/` is its own Cargo workspace on purpose: it embeds the built frontend, so
`cargo build --manifest-path src-tauri/Cargo.toml` requires `npm run build` first.
The root `cargo build --workspace` never touches it.

Crate package names are prefixed (`storagelens-core`, `storagelens-scan-macos`, …) so
they never collide with the standard `core` crate. The binary is plain `scanbench`.

### There is no CI — run the full suite locally before every merge

GitHub Actions was removed on purpose (#64). Nothing checks a PR for you, so the
author runs every gate below and states the result in the PR description:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
npm run typecheck && npm test && npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
# Windows code, from macOS: type-check only (C dependencies cannot cross-build).
cargo clippy -p storagelens-scan-windows --all-targets --all-features \
  --target x86_64-pc-windows-msvc -- -D warnings
```

**What this cannot cover:** from macOS, Windows code is only *type-checked*. Its
tests (`scan-windows`, `scanbench/tests/accuracy_windows.rs`, the app's command
tests) run only on a Windows machine. A change to Windows behaviour must say in its
PR whether it was run on Windows — never imply it was tested when it was not.

---

## 5. Invariants — violating these is a bug, not a tradeoff

### Sizes

1. **`size_alloc` is the only number ever presented as "space used".**
   `size_logical` may be shown only as "apparent size", clearly labelled. Sparse files
   (Docker.raw, pagefile) make these differ by 100×.
2. Hard links are counted **once**, deduped on `(dev, file_id)` when `nlink > 1`.
3. APFS/NTFS clones are **overcounted**. This is accepted and documented. Do not attempt
   to "fix" it with heuristics.

### Enumeration

4. **All filesystem enumeration goes through `StorageScanner`.** No `walkdir`, no
   `read_dir` recursion, no `std::fs::metadata` loops outside `scan-*` crates.
5. **No per-file syscalls beyond the bulk call.** Never `stat` in a loop. Never call
   `UTType`, `SHGetFileInfo`, or read the registry per file — build a cache once.
6. Classification happens **inline during the scan**, via the shared table in
   `core/categories.rs`. Both platforms must produce the same category for the same file.

### Deletion

7. **All deletion goes through `safety::trash`.** `std::fs::remove_file`,
   `remove_dir_all`, and `unlink` are forbidden in application code — only in tests,
   against temp dirs.
8. **Path guards run before every mutation.** Resolve symlinks/junctions, refuse
   `/System`, `C:\Windows`, `C:\Program Files`, bundle internals, and anything escaping
   the scanned root.
9. **Preflight re-stat** before deleting: size + mtime + file id must still match the
   scan, or the item is skipped. This is the TOCTOU guard.
10. **Dry-run receipt before execute.** `execute_deletion` must never be callable without
    a prior `preview_deletion` for the same plan id.

### Accounting

11. **`Σ category bytes == total scanned bytes`.** Assert it in tests. If it fails,
    something is unclassified or double-counted.
12. **All six grouping views must report the same root total.** If one disagrees, the
    synthetic tree builder is wrong.
13. **`Other` is always visible** with its file count and byte total. Never hide
    unclassified files.

---

## 6. Performance budget

| Metric | Budget |
|---|---|
| 1M files, cold cache | < 90 s |
| 1M files, warm cache | < 20 s |
| Peak RSS during scan | < 400 MB |
| Classification | ~50 ns/file (one hashmap lookup) |
| Synthetic grouping tree (1M nodes) | < 100 ms |
| UI during scan | always interactive |

Scan and hash threads must set I/O throttle (`setiopolicy_np` on macOS,
`FILE_FLAG_SEQUENTIAL_SCAN` on Windows). If a change exceeds budget, profile before
adding complexity — and say so rather than quietly regressing.

---

## 7. Testing

- **Never test against real user files.** Fixture trees are generated into temp dirs and
  deleted afterwards. No test may touch `~`, `C:\`, or any scanned path.
- **Fixture tree must include:** hard links, symlink loops, Windows junctions, sparse
  files, unicode/emoji/RTL names, 255-byte names, paths > 1,024 chars, permission-denied
  dirs, empty dirs, 200-level nesting.
- **Parser tests cross-check against `stat(2)` / `GetFileInformationByHandle`.** If the
  bulk-buffer offsets are wrong, sizes are silently wrong — this test is the only thing
  that catches it.
- **Safety tests get 100% coverage:** symlink escape, TOCTOU, path traversal, bundle
  refusal, manifest write failure, undo with a missing Trash item.
  Measure with `cargo +nightly llvm-cov -p storagelens-safety --branch --summary-only`
  (needs `cargo install cargo-llvm-cov` and a nightly toolchain with `llvm-tools-preview`;
  nightly is used only for this run).
  After changing `crates/safety`, also run `python3 scripts/safety-mutants.py`. It removes
  each guard in turn and fails unless the tests catch every removal. Safety tests touch
  only their own temp dirs; the single real-Trash round trip is an `#[ignore]`d test, run
  by hand.
- **Accuracy gate:** `scanbench` within **2%** of `du -skx` (macOS) /
  `Get-ChildItem -Recurse` (Windows), excluding snapshots, purgeable, clones, pagefile.

---

## 8. Code style

### Rust

- Edition 2021, `rustfmt` defaults.
- `#![forbid(unsafe_code)]` everywhere **except** `scan-macos` and `scan-windows`.
- Every `unsafe` block needs a `// SAFETY:` comment explaining the invariant upheld.
- No `unwrap()` / `expect()` in library code. `thiserror` for errors, propagate with `?`.
- Prefer plain structs and `&[T]` over trait objects in hot paths.
- Batch SQLite writes: one transaction per ~10k rows, WAL mode.

### TypeScript

- `strict: true`, no `any` (use `unknown` + narrowing).
- Function components only. State via `zustand`; no Redux, no context spaghetti.
- No UI component may hold more than ~200 rows in memory — use `react-window` +
  pagination without `OFFSET`: keyset cursors for forward-only lists, or a
  positional range over an order the backend keeps in memory
  (`catalog::listing`, as the Large Files table does) when the scrollbar must
  jump anywhere.
- All byte values formatted through one helper (`lib/format.ts`), never inline.

---

## 9. Git workflow

`main` is always releasable. Work happens on branches, one issue at a time, merged by PR.

### Branches

- **Never commit directly to `main`.** Every change arrives via a pull request.
- **One issue → one branch → one PR.** No bundling unrelated work.
- Branch name: `<type>/<issue-number>-<slug>`

  ```
  feat/12-findfirstfileex-walker
  fix/9-hardlink-dedupe
  perf/8-batch-writer
  test/35-safety-coverage
  docs/1-update-build-commands
  ```

- Branch from an up-to-date `main`:

  ```bash
  git switch main && git pull --ff-only
  git switch -c feat/12-findfirstfileex-walker
  ```

### Pull requests

- Title mirrors the eventual squash commit: `<type>(<scope>): <what changed>`
- Body must contain `Closes #<issue>` so the issue closes on merge.
- The full local suite (§4) passes; the PR description says so, and says which
  platforms the tests actually ran on.
- **Squash merge** into `main`, then delete the branch. No merge commits.
- If `main` moved ahead, rebase — do not merge `main` into your branch.

### Parallel work

The backlog is designed so several agents can work at once. To avoid collisions:

1. **Each issue lists `Touches`** — the files and directories it owns. Read it before
   starting.
2. **Two issues whose `Touches` overlap must not run concurrently.** Serialize them, or
   coordinate by rebasing and resolving in the PR.
3. **Do not edit files outside your issue's `Touches`.** If you genuinely must, say so
   explicitly in the PR description and note the dependency.
4. **Known contention points** — expect rebases when touching these:
   - root `Cargo.toml`
   - `src-tauri/src/commands.rs`
   - `ui/src/lib/api.ts`
   - `ui/src/routes/Map.tsx`
   - `crates/core/src/lib.rs`

A safe batch of five is one issue per area (`core`, `scan-macos`, `scan-windows`, `ui`,
`safety`), since areas rarely overlap.

### Commits

Conventional commits, scoped by crate or area:

```
feat(scan-macos): parse getattrlistbulk attribute buffer
fix(catalog): dedupe hard links on (dev, file_id)
perf(scan-windows): switch to FindExInfoBasic
test(safety): cover TOCTOU preflight rejection
docs(plan): record APFS clone overcounting as accepted
```

One logical change per commit. No `--no-verify`, no force-push to `main`.

### Working an issue

1. Read the issue, then the `PLAN.md` section it references.
2. Claim it (assign yourself) so a parallel agent doesn't duplicate work.
3. Branch from `main` using the naming convention above.
4. Implement, with tests.
5. `cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
6. Open a PR with `Closes #N` and a short summary of decisions made.
7. Squash merge once the full local suite (§4) passes.
8. If the work changed a design decision, update `PLAN.md` in the same PR.

---

## 10. Known traps

Do not rediscover these the hard way:

| Trap | Consequence |
|---|---|
| `getattrlistbulk` buffer alignment | Silently wrong sizes. Validate offsets against `stat(2)`. |
| macOS firmlinks (`/System/Volumes/Data`) | Double-counts the entire disk if walked twice. |
| Windows junctions | Infinite recursion. Check `FILE_ATTRIBUTE_REPARSE_POINT`. |
| Sparse files | Logical ≫ allocated. Show allocated. |
| APFS clones / NTFS dedupe | Overcounted, undetectable via standard attributes. Accepted. |
| `LastAccessTime` on Windows | Disabled by default. Never the sole basis for "unused". |
| `UTType` / `SHGetFileInfo` per file | Kills scan performance. Cache by extension. |
| `Docker.raw` | Sparse, and deleting it destroys all images. Report, don't delete. |

---

## 11. Definition of done

A change is done when:

- [ ] The full local suite in §4 passes, and the PR names the platforms it ran on
- [ ] New filesystem logic has a fixture-tree test
- [ ] Any size-affecting change was checked with `scanbench`
- [ ] No new network code, no new telemetry
- [ ] `PLAN.md` updated if a decision changed
