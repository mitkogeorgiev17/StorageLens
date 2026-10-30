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

Planning only. `PLAN.md` exists; the Cargo workspace has not been scaffolded yet.
See `PLAN.md` §16 for the milestone order. Start at M0.

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
  grouping.rs          synthetic tree builders (Folder/Type/App/Age/Size/Cleanliness)
crates/catalog/        SQLite schema, migrations, queries, tree arena, rollups
crates/scan-macos/     getattrlistbulk FFI, buffer parser, FSEvents
crates/scan-windows/   FindFirstFileExW walk, USN journal, registry
crates/analysis/       duplicate finder, app inventory, rules engine
crates/safety/         path guards, trash service, manifests, undo
crates/scanbench/      CLI binary — no UI, used to validate the scanners
src-tauri/             Tauri app, #[tauri::command] surface, event emitters
ui/                    React + TypeScript frontend
```

**Dependency direction is one-way:** `scan-*` → `core` ← `analysis` → `safety`.
`core` must never depend on a platform crate, and `analysis` must never call OS APIs
directly.

---

## 4. Build and test

> Scaffolding not yet created. These are the target commands; update this section at M0.

```bash
# Rust workspace
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# Scanner validation (no UI) — the important one
cargo run -p scanbench --release -- ~/Documents

# App (dev)
cd ui && npm install
npm run tauri dev

# UI only
cd ui && npm run typecheck && npm run lint

# Release build
npm run tauri build
```

CI runs `fmt`, `clippy -D warnings`, and `test`. Keep it green.

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
- **Accuracy gate:** `scanbench` within **2%** of `du -sxd` (macOS) /
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
  keyset pagination.
- All byte values formatted through one helper (`lib/format.ts`), never inline.

---

## 9. Commits

Conventional commits, scoped by crate or area:

```
feat(scan-macos): parse getattrlistbulk attribute buffer
fix(catalog): dedupe hard links on (dev, file_id)
perf(scan-windows): switch to FindExInfoBasic
test(safety): cover TOCTOU preflight rejection
docs(plan): record APFS clone overcounting as accepted
```

One logical change per commit. No `--no-verify`, no force-push to `main`.

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

- [ ] `cargo fmt`, `cargo clippy -D warnings`, `cargo test` pass
- [ ] New filesystem logic has a fixture-tree test
- [ ] Any size-affecting change was checked with `scanbench`
- [ ] No new network code, no new telemetry
- [ ] `PLAN.md` updated if a decision changed
