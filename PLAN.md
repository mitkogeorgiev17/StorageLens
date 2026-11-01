# StorageLens

**A personal storage mapper for macOS and Windows.**

Index every file on the machine, visualize it as a zoomable treemap, find safely
deletable junk, spot unused apps, and see exactly how much space can be reclaimed —
with a working undo.

| | |
|---|---|
| **Audience** | Me. Only me. |
| **Platforms** | macOS 14+ and Windows 10/11 (x64 + arm64) |
| **Distribution** | None. Build locally, run locally. |
| **Cost** | $0. No accounts, no services, no subscriptions. |
| **Network** | Never used. Fully offline. |
| **Status** | Plan — pre-implementation |

---

## Table of contents

1. [Scope](#1-scope)
2. [Stack decision](#2-stack-decision)
3. [Repository layout](#3-repository-layout)
4. [Architecture](#4-architecture)
5. [Data model](#5-data-model)
6. [Scanner — macOS](#6-scanner--macos)
7. [Scanner — Windows](#7-scanner--windows)
8. [Cross-platform size accounting](#8-cross-platform-size-accounting)
9. [File categorization & grouping](#9-file-categorization--grouping)
10. [Cleanup rules engine](#10-cleanup-rules-engine)
11. [App inventory & unused apps](#11-app-inventory--unused-apps)
12. [Duplicate detection](#12-duplicate-detection)
13. [UI & visualization](#13-ui--visualization)
14. [Tauri command surface](#14-tauri-command-surface)
15. [Safety model](#15-safety-model)
16. [Build schedule](#16-build-schedule)
17. [Testing & accuracy gates](#17-testing--accuracy-gates)
18. [Risks](#18-risks)
19. [The MVP cut](#19-the-mvp-cut)

---

## 1. Scope

### Does

| Capability | Detail |
|---|---|
| **Map** | Full-disk index of every file and folder, with real on-disk sizes |
| **Group** | The same index re-grouped by type, app, age, size, or cleanliness — not just folders |
| **Visualize** | Zoomable squarified treemap + sunburst, colored by file category |
| **Explain** | Top consumers by folder, by type, by app; "why is my disk full?" |
| **Clean** | Ranked, explained cleanup suggestions with risk levels and one-click Trash |
| **Apps** | Every installed app: size, last used, associated data, leftovers |
| **Duplicates** | Byte-identical files via size + BLAKE3, with keep-one recommendations |
| **Undo** | Every deletion is a Trash move with a manifest, fully reversible |

### Does not

- Not a backup tool, antivirus, or system monitor.
- **No automatic or scheduled deletion.** Every action is an explicit click.
- No touching system-protected paths, keychains, Photos/Music libraries, or Mail stores.
- No network access at all. No telemetry, no update checks, no crash uploads.
- No scanning of network shares or cloud mounts (removable drives: read-only, optional).

### The one-line pitch

> DaisyDisk's map, plus a receipt for every byte I delete.

---

## 2. Stack decision

### Tauri 2 — Rust core + React/TypeScript UI

```
Shell          Tauri 2
Core           Rust 2021 edition (one language for both scanners)
UI             React 19 + TypeScript + Vite
Treemap        d3-hierarchy (squarified) → <canvas>
Storage        SQLite via rusqlite (bundled — no external install)
Hashing        blake3
Parallelism    rayon
Watching       notify (FSEvents / ReadDirectoryChangesW)
Trash          trash crate (native Trash / Recycle Bin)
macOS APIs     libc + hand-written getattrlistbulk bindings
Windows APIs   windows crate (Win32)
```

### Why

The UI is the replaceable part. The valuable part is the two scanners, and they are
100% OS-specific. So the stack must let me write the scanners **once, in one language**,
with a thin shared UI on top.

| Option | Verdict |
|---|---|
| **Swift + SwiftUI** | Best Mac experience, but cannot ship on Windows. Two apps, two UIs. Disqualified. |
| **Tauri (Rust)** ✅ | One codebase, native window, ~12 MB, real syscalls in both backends. |
| **Electron** | 150 MB+ and ~300 MB RAM for a tool about saving disk. No benefit over Tauri. |
| **Go + local web UI** | Excellent fallback. Trivial cross-compile, single binary, zero toolchain pain — but opens in a browser tab, not an app. |
| **.NET + Avalonia** | Solid. C# is easier than Rust; P/Invoke does macOS fine. Weaker Windows-syscall ecosystem. |
| **Python + PySide** | Too slow for 1M-file scans, and two-platform packaging is misery. |

**Escape hatch:** the scanner sits behind a `StorageScanner` trait. If Rust becomes a
slog, the core can be ported to Go without touching the schema, the rules, or the UI.

### Dependencies (pin exact versions at setup)

| Crate / package | Purpose |
|---|---|
| `tauri` 2.x | App shell, IPC, bundler |
| `rusqlite` (feature `bundled`) | SQLite, no system dependency |
| `rayon` | Parallel directory walking |
| `blake3` | Fast content hashing |
| `trash` | Cross-platform Trash / Recycle Bin |
| `notify` | Filesystem watching |
| `serde` / `serde_json` | IPC + rule catalog |
| `thiserror` | Error types |
| `tracing` / `tracing-subscriber` | Local logs |
| `windows` 0.5x | Win32 bindings |
| `libc` | POSIX + `getattrlistbulk` FFI |
| `d3-hierarchy` | Treemap layout |
| `zustand` | UI state |
| `react-window` | Virtualized tables |
| `tauri-plugin-dialog` | Native folder chooser for the scan picker; only `dialog:allow-open` is granted |

---

## 3. Repository layout

```
~/Desktop/StorageLens/
├─ PLAN.md                     ← this file
├─ Cargo.toml                  ← workspace root
├─ crates/
│  ├─ core/                    Entry, Volume, ScanOptions, StorageScanner trait, errors
│  │                           categories.rs (shared extension table), tags.rs, grouping.rs
│  ├─ catalog/                 SQLite schema, migrations, queries, tree arena
│  ├─ scan-macos/              getattrlistbulk FFI, FSEvents
│  ├─ scan-windows/            bulk dir-query walk, USN journal, registry
│  ├─ analysis/                Rollups, duplicates, app inventory, rules engine
│  ├─ safety/                  Path guards, trash service, manifests, undo
│  └─ scanbench/               CLI binary (no UI) — built first
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ tauri.conf.json
│  └─ src/
│     ├─ main.rs
│     ├─ commands.rs           #[tauri::command] surface
│     └─ events.rs             scan://progress etc.
└─ ui/
   ├─ package.json
   ├─ vite.config.ts
   ├─ index.html
   └─ src/
      ├─ main.tsx
      ├─ routes/               Overview, Map, LargeFiles, Apps, Cleanup (+ duplicates)
      ├─ components/           Treemap, Sunburst, Gauge, FileTable, RuleCard, RiskBadge
      └─ lib/                  api.ts, format.ts, colors.ts, treemap.ts
```

`scanbench` is a separate binary in the same workspace, so it shares every crate the app
uses. Same code path, no drift.

---

## 4. Architecture

```
┌───────────────────────────────────────────────────────────────┐
│  React UI (identical on both OSes)                            │
│  Overview │ Map │ Large Files │ Apps │ Cleanup │ Duplicates   │
└──────────────────────────┬────────────────────────────────────┘
                           │ Tauri IPC (JSON) + events
┌──────────────────────────▼────────────────────────────────────┐
│  Core (shared Rust)                                           │
│  Catalog (rusqlite) · RulesEngine · Rollups · Duplicates      │
│  Safety (trash + manifest + undo) · Progress events           │
└──────────────────────────┬────────────────────────────────────┘
                           │  trait StorageScanner
        ┌──────────────────┴──────────────────┐
┌───────▼─────────┐                  ┌────────▼────────┐
│  macOS backend  │                  │ Windows backend │
│  getattrlistbulk│                  │ dir bulk query  │
│  FSEvents       │                  │ USN journal     │
│  NSWorkspace    │                  │ Registry/Win32  │
└─────────────────┘                  └─────────────────┘
```

### The trait

```rust
pub struct Entry {
    pub parent: u32,          // index into the node arena; 0 = root
    pub name: String,
    pub kind: EntryKind,      // File | Dir | Symlink | Other
    pub size_alloc: u64,      // bytes actually on disk   ← what we display
    pub size_logical: u64,    // apparent file size
    pub dev: u32,             // volume identity
    pub file_id: u64,         // inode / file index
    pub nlink: u32,
    pub attrs: u32,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub hardlink_dup: bool,   // row kept, but not counted in rollups
}

pub trait StorageScanner: Send + Sync {
    fn volumes(&self) -> Vec<Volume>;
    fn scan(
        &self,
        roots: &[PathBuf],
        sink: &mut dyn EntrySink,
        cancel: CancelToken,
        progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<ScanResult>;
    fn platform_cleanup_targets(&self) -> Vec<PlatformTarget>;
    fn installed_apps(&self) -> Vec<AppInfo>;
}
```

Everything above this trait — schema, rules, rollups, dedupe, safety, UI — is written once.

Scanners do not return a `Vec<Entry>`. They stream entries into an `EntrySink`
(implemented by the catalog's batched writer), so a million-file scan never holds the
whole tree in memory before the first row reaches SQLite. `ScanResult` carries only the
summary and the list of skipped directories.

`Entry` carries only `parent`, not its own arena index, so the sink assigns indices in
acceptance order. **Entries must therefore arrive in pre-order** — a parent before any of
its children, root first — which is the order a depth-first walk already produces.

The sink is passed to `scan` as `&mut dyn EntrySink`, not stored on the scanner, because
`EntrySink` is `!Send` (it wraps a SQLite connection). A parallel scanner forwards `Entry`
values from its workers to the calling thread, which is the only writer. Sibling subtrees
may interleave, but the writer accepts a directory's own entry before dispatching the
enumeration of that directory, so parents are always persisted before their children and
the arena indices stay parent-first. Only the first root is the arena's node 0; M1 scans
exactly one root per call.

---

## 5. Data model

SQLite in the OS app-data directory. One database, one schema, both platforms.

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA cache_size   = -131072;   -- 128 MB

CREATE TABLE scan_session (
  id           INTEGER PRIMARY KEY,
  platform     TEXT    NOT NULL,      -- 'macos' | 'windows'
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  status       TEXT    NOT NULL,      -- running|complete|cancelled|partial
  files_seen   INTEGER NOT NULL DEFAULT 0,
  bytes_seen   INTEGER NOT NULL DEFAULT 0,
  dirs_skipped INTEGER NOT NULL DEFAULT 0,
  elevated     INTEGER NOT NULL DEFAULT 0,
  root         TEXT                   -- absolute scanned path; NULL before schema v2
);

CREATE TABLE entries (
  id           INTEGER PRIMARY KEY,
  scan_id      INTEGER NOT NULL REFERENCES scan_session(id),
  parent_id    INTEGER NOT NULL,      -- 0 = volume root
  name         TEXT    NOT NULL,
  ext          TEXT,                  -- lowercased, last segment only ("gz" for .tar.gz)
  kind         INTEGER NOT NULL,      -- 1 file, 2 dir, 3 link, 4 other
  size_alloc   INTEGER NOT NULL,
  size_logical INTEGER NOT NULL,
  dev          INTEGER NOT NULL,
  file_id      INTEGER NOT NULL,
  nlink        INTEGER NOT NULL,
  attrs        INTEGER NOT NULL,
  created      INTEGER,
  modified     INTEGER,
  accessed     INTEGER,
  category     TEXT,                  -- one of PLAN §9.5: images|video|audio|documents|
                                      -- spreadsheets|presentations|archives|code|data|
                                      -- fonts|3d|apps|disk-images|caches|app-data|
                                      -- system|developer|other
  tags         INTEGER NOT NULL DEFAULT 0,   -- u64 bitmask, see §9.2
  hardlink_dup INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_entries_parent ON entries(scan_id, parent_id);
CREATE INDEX idx_entries_size   ON entries(scan_id, size_alloc DESC);
CREATE INDEX idx_entries_cat    ON entries(scan_id, category, size_alloc DESC);
CREATE INDEX idx_entries_ext    ON entries(scan_id, ext);

-- Materialized post-order subtree totals. Built once after a scan.
CREATE TABLE dir_rollup (
  scan_id       INTEGER NOT NULL,
  dir_id        INTEGER NOT NULL,
  total_alloc   INTEGER NOT NULL,
  total_logical INTEGER NOT NULL,
  file_count    INTEGER NOT NULL,
  PRIMARY KEY (scan_id, dir_id)
);

CREATE TABLE rule_hit (
  id         INTEGER PRIMARY KEY,
  scan_id    INTEGER NOT NULL,
  rule_id    TEXT    NOT NULL,
  path       TEXT    NOT NULL,
  entry_id   INTEGER,
  bytes      INTEGER NOT NULL,
  risk       TEXT    NOT NULL,        -- safe | review | keep | never
  confidence REAL    NOT NULL,
  selected   INTEGER NOT NULL DEFAULT 0,
  UNIQUE(scan_id, rule_id, path)
);

CREATE TABLE trash_log (
  id            INTEGER PRIMARY KEY,
  performed_at  INTEGER NOT NULL,
  original_path TEXT    NOT NULL,
  trashed_name  TEXT,
  bytes         INTEGER NOT NULL,
  rule_id       TEXT,
  manifest_path TEXT,
  restored_at   INTEGER
);

-- v4 (#28): smart collections — dropped again in v5 (DROP TABLE collection).
CREATE TABLE collection (
  id         INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  query      TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

The catalog keeps one **read** connection. A scan writes through a second connection
with its own batch transaction, so WAL keeps the UI reading the previous scan while the
next one is being written. Only one writer may exist at a time (row ids are allocated
from `MAX(id) + 1`); scans are serialized by the app.

### In-memory tree for rendering

```rust
#[repr(C)]
struct Node {             // 40 bytes, asserted at compile time
    parent: u32,          // NIL (u32::MAX) for the root
    first_child: u32,
    next_sibling: u32,
    name_offset: u32,     // into a shared string blob
    size_alloc: u64,      // own bytes
    total_alloc: u64,     // subtree bytes, filled by the rollup
    kind: u8,
    category: u8,         // index into Category::ALL
    flags: u8,            // hard-link duplicate, later risk bits
    _reserved: [u8; 5],
}
```

1M nodes ≈ 40 MB + ~30 MB string blob (measured: 76 MB including the logical-size and
rollup arrays, loaded in 0.75 s). The treemap lays out against this arena, never
against SQLite, so zooming is instant. Node `i` is SQL row `base_id + i`: the writer
assigns one scan's ids contiguously in pre-order, which `Arena::load` verifies.

### Rollups

Post-order DFS after the scan completes, writing `dir_rollup` in one transaction.
Because ids are pre-order, one reverse pass over the arena is that post-order — no
recursion. 1M rows: 0.13 s to write (release build). Recompute only for dirty subtrees on incremental refresh.

---

## 6. Scanner — macOS

### 6.1 Enumeration

`getattrlistbulk(2)` — one syscall returns attributes for **many** directory entries.
This is the single biggest performance decision on macOS; `FileManager.enumerator` or
`readdir + stat` are 5–10× slower.

```rust
#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved:    u16,
    commonattr:  u32,
    volattr:     u32,
    dirattr:     u32,
    fileattr:    u32,
    forkattr:    u32,
}

extern "C" {
    fn getattrlistbulk(
        dirfd: libc::c_int,
        alist: *mut AttrList,
        attr_buf: *mut libc::c_void,
        buf_size: libc::size_t,
        options: u64,
    ) -> libc::c_int;
}
```

Requested attributes:

```c
commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_DEVID
           | ATTR_CMN_FILEID | ATTR_CMN_OBJTYPE | ATTR_CMN_FLAGS
           | ATTR_CMN_CRTIME | ATTR_CMN_MODTIME | ATTR_CMN_ACCTIME;

fileattr   = ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE
           | ATTR_FILE_LINKCOUNT | ATTR_FILE_IOBLOCKSIZE;

dirattr    = ATTR_DIR_ENTRYCOUNT;

options    = FSOPT_PACK_INVAL_ATTRS | FSOPT_NOFOLLOW;
```

- `FSOPT_NOFOLLOW` → symlink loops are impossible; links are recorded, never followed.
- `FSOPT_PACK_INVAL_ATTRS` → missing attributes come back zero-length, so "0 bytes" and
  "attribute absent" are distinguishable.

### 6.2 Buffer parsing — the part that bites

Each record is `u32 length` followed by attributes **in bitmap order**: returned_attrs,
then common, then dir, then file, then fork. Within each group, ascending bit order.

Two things to get right:

1. `ATTR_CMN_RETURNED_ATTRS` is always first and tells you which attributes actually
   exist for this entry. Parse it before anything else.
2. **Alignment is under-documented.** Scalars are 4-byte aligned; `struct timespec` is
   8-byte on arm64. Validate every field offset against `stat(2)` on the fixture tree
   before trusting the parser. Do not hand-wave this.

Mitigation: write a `parse_bulk_buffer` unit test that compares parsed output against
`stat(2)` for every entry in a fixture directory. If they diverge, the offsets are wrong.

### 6.3 macOS traps

| Trap | Handling |
|---|---|
| **Full Disk Access** | Required for `~/Library/Mail`, other apps' containers, TM data. **No API to request it** — deep-link to `x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles` and poll. Probe `~/Library/Mail` for `EPERM` to detect status. |
| **Firmlinks** | `/Users` → `/System/Volumes/Data/Users`. Walking both double-counts the entire disk. **Skip all `/System/Volumes/*`.** |
| **Hard links** | Dedupe on `(devid, fileid)` when `nlink > 1`. Count once, attribute to first path seen. Every name still gets a row; later names carry their true size but set `hardlink_dup`, and rollups exclude them. |
| **APFS clones** | Shared extents, both report full size. Not exposed by `getattrlist`. Accept overcount. |
| **Sparse files** | `Docker.raw` = 100 GB logical, 4 GB allocated. Always show **allocated**. |
| **Local TM snapshots** | Invisible to `du`, real space. `tmutil listlocalsnapshots /`. |
| **Purgeable space** | `volumeAvailableCapacityForImportantUsage` vs `volumeAvailableCapacity`. |
| **TCC user folders** | Without `NSDesktopFolderUsageDescription`, `NSDocumentsFolderUsageDescription`, `NSDownloadsFolderUsageDescription`, those scans silently return nothing. |
| **Package bundles** | `.app` is a directory. Walk it normally, but mark the bundle root so "delete app" works on the whole thing. |

### 6.4 Default skip list

Skipped with `SkipReason::SystemPath` (the directory keeps its own row, its subtree is
never enumerated):

```
/System/Volumes                (all of Data, VM, Preboot, Update, xarts, iSCPreboot, Hardware)
/dev
/Volumes                      (recurse per-volume instead)
/.Spotlight-V100
/.fseventsd
/.Trashes
```

Walked and reported in the tree, but never offered for deletion:

```
/.DocumentRevisions-V100      (can be GBs)
/private/var/vm               (swap and the sleep image)
```

They are real space, so hiding them from the scan would make the total wrong. The safety
layer's path guards refuse to delete them; M4 surfaces each as a `system (report)` rule.

A directory on another device is reported as `SkipReason::MountPoint` unless
`cross_filesystems` is set. The scan root itself is always entered, even if it is on the
skip list.

### 6.5 Speed

8 rayon workers, each with a directory queue and a 4–8 MB batch buffer. One writer thread
into SQLite, one transaction per ~10k rows. Apply
`setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_THREAD, IOPOL_THROTTLE)` on scan threads so
the machine stays usable while scanning.

| Metric | Target |
|---|---|
| 1M files, warm cache | < 20 s |
| 1M files, cold cache | < 90 s |
| 3M files | < 4 min |
| Peak RSS | < 400 MB |
| UI interactive during scan | Always |

### 6.6 Incremental refresh

`FSEvents` marks dirty subtrees; re-stat only those. Directory `mtime` is the cheap
invalidation signal. Full rescan is the fallback, not the default.

---

## 7. Scanner — Windows

### 7.1 v1 — bulk directory query (no elevation)

The pragmatic choice: one bulk call per directory batch, no admin rights.

```rust
CreateFileW(dir, FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None)

GetFileInformationByHandleEx(handle, FileIdBothDirectoryInfo, &mut buf, 64 * 1024)
// repeat until ERROR_NO_MORE_FILES
```

Each `FILE_ID_BOTH_DIR_INFO` record carries the name, attributes, the three timestamps,
`EndOfFile` (logical size), `AllocationSize`, the 64-bit `FileId`, and the reparse tag
(in `EaSize` when `FILE_ATTRIBUTE_REPARSE_POINT` is set).

**Why not `FindFirstFileExW`** (the original plan, shipped in #12): `WIN32_FIND_DATAW`
has no file id and no allocation size, so hard-link dedupe and bytes-on-disk would have
needed a handle per file — a per-file syscall that invariant 5 forbids. The directory
query returns both in the same single call (#13).

**The one per-file call:** a record's `AllocationSize` overstates compressed and sparse
files, so for files flagged `FILE_ATTRIBUTE_COMPRESSED` / `FILE_ATTRIBUTE_SPARSE_FILE`
the walker asks `GetCompressedFileSizeW`. Only flagged files pay it; they are rare
outside system folders.

**No link count** is in the record, so the walker records every regular file's id
(~16 MB per 1M files) and dedupes on it — on **NTFS only**, where the 64-bit id is
unique. ReFS ids are 128-bit and FAT has none, so those volumes are not deduped.

### 7.2 v2 — MFT / USN journal (elevated, 10× faster)

`FSCTL_ENUM_USN_DATA` on `\\.\C:` walks the Master File Table and returns **every file on
the volume in 1–3 seconds** — name, file reference, parent reference.

```
CreateFileW(L"\\\\.\\C:", GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
            OPEN_EXISTING, 0, NULL)

DeviceIoControl(h, FSCTL_ENUM_USN_DATA,
                &MFT_ENUM_DATA_V0 { StartFileReferenceNumber, LowUsn, HighUsn },
                sizeof, buf, bufsize, &bytes, NULL)
```

USN records carry **no size**, so pair with `FSCTL_GET_NTFS_FILE_RECORD` (parse the `$DATA`
attribute from the raw MFT record) or fall back to `GetCompressedFileSizeW` per file.

Build this only if v1 feels slow. It's a deep rabbit hole (MFT record parsing, attribute
lists, `$ATTRIBUTE_LIST` for fragmented records) with real payoff.

### 7.3 Windows traps

| Trap | Handling |
|---|---|
| **Allocated size** | `AllocationSize` from the directory record; `GetCompressedFileSizeW` only for compressed and sparse files, whose record overstates it (§7.1). |
| **Sparse files** | `hiberfil.sys`, `pagefile.sys`, VM disks. Same logical-vs-allocated split as Mac. |
| **Junctions / reparse points** | Must skip or you loop forever. Check `FILE_ATTRIBUTE_REPARSE_POINT`. |
| **Long paths** | Use `\\?\` prefixes throughout. |
| **Permissions** | Unelevated, parts of `C:\` are unreadable. Report "not scanned" — never silently under-report. Offer a "relaunch elevated" button. |
| **`$Recycle.Bin`, `System Volume Information`** | Skip; report separately. |
| **Hard links** | Dedupe on `(volume serial, FileId)` from the directory record, NTFS only (§7.1). |
| **Case-insensitive paths** | Normalize comparisons; never dedupe paths by exact string. |
| **UNC / mapped drives** | Skip by default; scanning `\\server\share` over SMB is pathologically slow. |

### 7.4 Speed

The bulk directory query (§7.1) with 8 rayon workers: roughly 1M files in 30–90 s (vs 1–3 s for MFT).
Good enough for v1.

---

## 8. Cross-platform size accounting

The one thing that will make the app look broken if it's wrong.

**Rule: always display `size_alloc` (bytes on disk), never `size_logical` (apparent
size).** Show both in the detail pane, labeled clearly.

| Concept | macOS | Windows |
|---|---|---|
| Bytes on disk | `ATTR_FILE_ALLOCSIZE` | `AllocationSize` (dir record); `GetCompressedFileSizeW` if compressed/sparse |
| Apparent size | `ATTR_FILE_TOTALSIZE` | `nFileSizeHigh/Low` |
| Identity | `(ATTR_CMN_DEVID, ATTR_CMN_FILEID)` | `(volume serial, FileId)` — same value as `nFileIndexHigh/Low` |
| Hard link count | `ATTR_FILE_LINKCOUNT` | not in the bulk record; every file id is tracked instead |
| Compression | automatic in `ALLOCSIZE` | `GetCompressedFileSizeW`, flagged files only |
| Dedupe / clones | invisible → overcount | invisible → overcount |

Both platforms overcount clones/reflinks. That is acceptable and documented.

---

## 9. File categorization & grouping

### 9.1 Two axes, not one

A filesystem is a tree. But *"what is this file?"* is orthogonal to *"where does it
live?"* — and people ask both questions about their disk.

- **Location axis** — the real directory hierarchy. Answers *"where is my space going?"*
- **Category axis** — what each file actually is. Answers *"what is eating my space?"*

The index is built once and feeds both. Categorization costs one hashmap lookup per file
during the scan, so there is no second pass and no extra I/O.

### 9.2 Categories are tags, not one field

A file has one *type* but many *properties*. "A 4 GB `.mp4` in `~/Downloads`, untouched
for two years" is Video + Huge + Stale + Installer-adjacent, all at once.

Model it as a bitmask:

```rust
pub struct Tags(u64);          // ≤ 64 tags, 8 bytes per file

pub const T_IMAGE:       u64 = 1 << 0;
pub const T_VIDEO:       u64 = 1 << 1;
pub const T_AUDIO:       u64 = 1 << 2;
pub const T_DOCUMENT:    u64 = 1 << 3;
pub const T_ARCHIVE:     u64 = 1 << 4;
pub const T_CODE:        u64 = 1 << 5;
pub const T_APP:         u64 = 1 << 6;
pub const T_CACHE:       u64 = 1 << 32;   // path-derived, not type-derived
pub const T_DEV_ARTIFACT:u64 = 1 << 33;
pub const T_INSTALLER:   u64 = 1 << 34;
pub const T_BACKUP:      u64 = 1 << 35;
pub const T_SYSTEM:      u64 = 1 << 36;
pub const T_ORPHAN:      u64 = 1 << 37;
pub const T_HUGE:        u64 = 1 << 38;
pub const T_STALE:       u64 = 1 << 39;
pub const T_DUPLICATE:   u64 = 1 << 40;
```

Two storage layers, for two different jobs:

| Where | What | Why |
|---|---|---|
| SQL `category TEXT` (indexed) | The single primary type | Lets SQLite answer "total video bytes" without scanning |
| In-memory `tags: u64` | Everything else | 1M files = 8 MB; bitmask filtering over the arena is instant |

Do not put the bitmask in SQL and expect an index to help — `WHERE tags & ?` cannot use a
B-tree. Primary type goes in a column; secondary tags stay in the arena.

### 9.3 Type detection — the same table on both OSes

The naive approach is to ask the OS: `UTType` on macOS, `HKEY_CLASSES_ROOT` on Windows.
Both work, and both are the wrong default:

- `UTType(filenameExtension:)` is a LaunchServices round trip. Calling it per file is a
  disaster; calling it once per *distinct extension* is fine, but it still means two
  different answers for the same file on two machines.
- `SHGetFileInfo` on Windows is a shell call — it can hit the disk and the registry. Per
  file, unusable at 1M files.

**Recommendation: one curated static table, shared by both platforms.**

```
core/src/categories.rs
  ~600 entries: "mp4" => Video, "heic" => Image, "ts" => Code,
                "psd" => Image, "dmg" => Disk image, "epub" => Document, ...
```

- Deterministic, identical on both OSes, no syscalls, ~50 ns per lookup.
- Covers ~99% of real-world files.
- The OS-specific sources become an **optional enrichment layer** — used only to fill gaps
  in the static table, never as the primary path.
  On Windows, `scan-windows::registry::load()` reads `HKCR\.ext` (`PerceivedType`, then
  `Content Type`) once at startup, and `Category::install_overrides` accepts only
  extensions the table maps to `Other`. The scan loop never touches the registry.

This is the difference between a tool that says "Video 180 GB" on both machines and one
that says it on one and "Other 180 GB" on the other.

### 9.4 Extension traps

| Trap | Handling |
|---|---|
| **Compound extensions** | `.tar.gz`, `.tar.bz2`. Categorize by the last segment; display the full compound. |
| **No extension** | Magic-byte sniff on the first 4 KB: `%PDF`, `\x89PNG`, `PK\x03\x04`, `ftyp` at offset 4. |
| **Wrong extension** | Optional sniff pass over files that landed in "Other" above 10 MB. Cheap, catches `.txt` files that are really PDFs. |
| **Dotfiles** | `.gitignore`, `.env`, `.zshrc` → Code/Config, not Unknown. |
| **Bundles / packages** | `.app`, `.photoslibrary`, `.fcpbundle`, `.logicx`, `.xcodeproj` are *directories*. Classify the bundle as one unit; do not descend into it for categorization purposes. |
| **Case** | `.JPG` ≡ `.jpg`. Lowercase the extension before lookup. |
| **Windows hidden/system** | `desktop.ini`, `Thumbs.db` → System, never suggest deleting system. |

### 9.5 Category taxonomy

Keep it small and stable — 18 top-level buckets (14 by extension, 4 by location), with
subcategories for display only:

```
Images        jpg png heic raw tiff psd svg ...
Video         mp4 mov mkv avi webm ...
Audio         mp3 flac wav aac m4a ...
Documents     pdf docx txt epub md ...
Spreadsheets  xlsx csv numbers ...
Presentations pptx key ...
Archives      zip tar gz 7z rar ...
Code          swift rs ts py js java c cpp sh ...
Data          json sqlite db parquet ...
Fonts         ttf otf woff ...
3D / CAD      obj fbx blend step ...
Apps          .app .exe .msi
Disk images   dmg iso vhd vhdx
Caches        by location: ~/Library/Caches, .cache, GPUCache, AppData\Local\Temp ...
App data      by location: ~/Library/Application Support, Containers, AppData ...
System        by location: /System, /usr, /private, /Library, C:\Windows, ProgramData
Developer     by location: node_modules, target/debug, .rustup, DerivedData ...
Other         everything unclassified
```

`Other` must always be visible, with a file count and byte total. Hiding it hides bugs.

**Decision (2026-11-01, #95, user-approved): location before extension.** On a real
full-disk scan of a Mac, `Other` was 52% (71 GB). Nearly all of it had no telling
extension (none, `.o`, `.rlib`, `.dat`, `.car`, …) but an unmistakable location: app
caches, app data, files inside `.app` bundles, the OS, build output. So four location
categories were added, and a directory's location category applies to every file below
it, whatever the extension: a `.jpg` in `~/Library/Caches` is Caches, not Images. Files
inside an `.app` bundle or `Program Files` are Apps. Rules live in `core::location`
(pure functions of path components, so Windows rules are tested on macOS too) and are
applied by the catalog writer as entries arrive, with no extra pass. A directory
takes:
1. its parent's category if that is Apps, System, Developer or Caches;
2. else a category from its name at any depth: cache names, `node_modules` /
   `.rustup` / `DerivedData` / `target/{debug,release}` and friends, or `*.app`. Inside
   App data, only cache names refine;
3. else App data if its parent is App data;
4. else a fixed OS location: `~/Library/<child>` (Caches/Logs → Caches, Developer →
   Developer, Mobile Documents/CloudStorage → none, anything else → App data),
   `/System` `/usr` `/private` `/Library` … → System (with `/private/var/folders` →
   Caches and `/Library/Developer` → Developer), `/opt/homebrew` → Developer;
   `C:\Windows`, `ProgramData` → System, `Program Files` → Apps,
   `%USERPROFILE%\AppData\Local\Temp` → Caches, `AppData\*` → App data;
5. else nothing: extensions decide.

A bare `target` folder is not claimed (too common a name); its `debug` / `release`
children are. `.bin` is no longer a disk image: it is a generic binary suffix and put
136k firmware and model files into Disk images. Scans recorded before this change keep
their extension-only categories until rescanned.

### 9.6 Grouping views — the actual payoff

Same treemap component. Different tree handed to it. **The layout code does not change.**

| Grouping | Root level | Question it answers |
|---|---|---|
| **Folder** | volume → directories | "Where is my space going?" |
| **Type** | Images, Video, Documents, … | "How much of my disk is video?" |
| **App** | app name → its support data | "Which app is the hog?" |
| **Age** | <1m, 1–6m, 6–12m, 1–2y, 2y+ | "What's abandoned?" |
| **Cleanliness** | Safe, Review, Keep, Never | "What can I delete?" |
| **Size** | >10 GB, 1–10 GB, 100 MB–1 GB, … | "What are the whales?" |

Each view is a **synthetic tree** built in memory from the arena:

```rust
// Type view:  root → category → extension → files
// Age view:   root → bucket → files
// App view:   root → app → its satellites → files
fn build_grouping(arena: &Arena, by: Grouping) -> SyntheticTree;
```

1M nodes → synthetic tree in well under 100 ms. No SQL, no rescans.

Built in `crates/analysis/src/grouping.rs` rather than `core`, because it reads the
catalog's arena. Measured on 1M nodes (release): Folder 19 ms, Type 62 ms, App 9 ms,
Age 12 ms, Size 10 ms, Cleanliness 7 ms. App ownership is a path heuristic for now:
`*.app` bundles, plus `~/Library/{Caches,Application Support,Containers,…}`,
`Program Files`, `AppData` and `ProgramData` children, matched to a bundle by name
(`com.google.Chrome` → Google Chrome). The app inventory (#36/#37) replaces it with
real bundle ids. Until rules exist (#31), Cleanliness puts every file under
Unclassified.

Because the treemap, the breadcrumb, the hover tooltip, and the zoom interaction are all
generic over "a tree of sized nodes," all six views are nearly free once the first one
works. **This is the single highest-leverage design decision in the UI.**

### 9.7 Cross-filtering and smart collections

Filters compose into queries rather than trees:

```
Type = Video
  AND Size > 500 MB
  AND Age > 1 year
  AND Path not under ~/Movies
```

So the Map screen gets a filter bar with chips, and the result can be shown either as a
flat virtualized table or re-grouped into a treemap. Saved as **smart collections** — a
JSON query that re-evaluates on every scan.

That's how "find every video over 500 MB I haven't touched in a year" becomes one click
instead of a manual hunt.

**Built so far (#26):** the Map's toolbar has the grouping selector and chips for type (any
number), minimum size, age, and location — a file must pass every chip. `Filter` is
`{ categories, minSize, olderThanDays, within }`, where `within` is a folder's entry id
("Path under"). In the Folder view the breadcrumb is the location; switching to another
grouping inside a folder sets `within` to it. The view lives in the URL hash
(`#map?by=type&type=video&size=1g&age=1y&in=42:Downloads`), so back/forward restore it.
"Path not under" and the cleanliness chip are not built; the cleanliness chip waits for the
rules engine (#31), and the Cleanliness grouping is disabled until rules give files a verdict.

**Smart collections (#28):** built, then dropped from the product; schema v5 drops the
`collection` table.

### 9.8 Performance

| Concern | Answer |
|---|---|
| Classification during scan | One hashmap lookup per file, ~50 ns. Immeasurable against the syscall. |
| Never do | Call `UTType`, `SHGetFileInfo`, or read the registry per file. |
| Magic-byte sniffing | Separate throttled pass, only for "Other" + extensionless + > 1 MB. |
| Synthetic grouping trees | Built in memory, < 100 ms for 1M nodes. |
| Re-grouping on toggle | Instant; no disk I/O, no SQL. |

### 9.9 Accuracy gate

- Target: **≥ 95% of bytes** land in a named category.
- **Invariant: `Σ category bytes == total scanned bytes`.** Assert this in tests. If it
  doesn't hold, either a file is unclassified or a size was double-counted — both are bugs.
- A wrong category is worse than an honest "Other". Never guess silently.

---

## 10. Cleanup rules engine

Rules are **JSON**, tagged by platform, so one catalog serves both OSes and can be edited
without recompiling.

```json
{
  "id": "dev.node_modules",
  "platform": "any",
  "title": "node_modules in stale projects",
  "glob": "**/node_modules",
  "exclude": ["**/node_modules/**/node_modules"],
  "kind": "regenerable",
  "risk": "safe",
  "confidence": 0.95,
  "stale_days": 90,
  "explain": "Dependencies can be reinstalled with npm install.",
  "action": "trash"
}
```

### Rule kinds

| Kind | Meaning | Default selected |
|---|---|---|
| `regenerable` | Rebuilt automatically on next use | yes |
| `stale` | Untouched for N days | yes |
| `duplicate` | Byte-identical to another file | no (pick keep-one) |
| `leftover` | Belongs to something that no longer exists | no |
| `valuable` | Real data (backups, mail) | no, with warning |
| `system` | Report only, never deletable | never |

### macOS catalog

| Rule | Path | Risk | Typical |
|---|---|---|---|
| Xcode Derived Data | `~/Library/Developer/Xcode/DerivedData` | safe | 5–50 GB |
| Xcode iOS DeviceSupport | `~/Library/Developer/Xcode/iOS DeviceSupport` | safe | 5–40 GB |
| Xcode Archives | `~/Library/Developer/Xcode/Archives` | review | 1–20 GB |
| Unavailable Simulators | `~/Library/Developer/CoreSimulator/Devices` | safe | 2–20 GB |
| User caches | `~/Library/Caches/*` 30d+ | safe | 1–15 GB |
| App container caches | `~/Library/Containers/*/Data/Library/Caches` | safe | 1–20 GB |
| Logs | `~/Library/Logs`, `/Library/Logs` | safe | <2 GB |
| Trash | `~/.Trash` | review | varies |
| Old installers | `~/Downloads/*.{dmg,pkg,zip}` 90d+ | review | 1–20 GB |
| Old downloads | `~/Downloads` untouched 1y+ | review | varies |
| iOS backups | `~/Library/Application Support/MobileSync/Backup` | valuable | 10–100 GB |
| Mail attachments | `~/Library/Mail/**/Attachments` | valuable | 1–20 GB |
| Docker disk image | `~/Library/Containers/com.docker.docker/.../Docker.raw` | review | 10–100 GB |
| Local TM snapshots | `tmutil listlocalsnapshots /` | safe | 5–50 GB |
| Homebrew cache | `~/Library/Caches/Homebrew` | safe | 1–10 GB |
| Orphaned app support | `~/Library/Application Support/<bundleid>` | review | 1–10 GB |
| Orphaned preferences | `~/Library/Preferences/<bundleid>.plist` | review | <1 GB |
| Document revisions | `/.DocumentRevisions-V100` | system (report) | 1–10 GB |
| Swap / sleep image | `/private/var/vm` | system (report) | — |

**Docker special case:** `Docker.raw` is sparse, so the file is much smaller on disk than
its logical size, and deleting it nukes every image and container. Never delete it — show
a card that says "run `docker system prune`" with the current `docker system df` output.

**Docker contents (#168).** `docker system prune` keeps tagged unused images, volumes and
most build cache, so Cleanup has a Docker section that lists what is inside through the
local docker CLI (`docker system df -v`, local socket only, no network): containers,
images, volumes, and build cache split into unused-30+-days and the rest. Unused items can
be removed through Docker (`container rm`, `image rm`, `volume rm`, `builder prune`). It is
permanent — Docker has no Trash — so it gets its own confirmation, and each target is
checked against a fresh listing first. Unused and older than 30 days starts selected;
volumes never do. `Docker.raw` itself is still never deleted.

**Built (#32).** `crates/analysis/rules/macos.json` has 20 rules, each with a fixture and
an exact byte total in `tests/rules_macos.rs`. Where it differs from the table above:

- **Report-only, because StorageLens moves things to the Trash and nothing else (§15.1):**
  - **Trash** (`~/.Trash`): emptying it would also end undo.
  - **Local TM snapshots:**
    - They are not files, so no glob can see them.
    - They are listed by `scan_macos::snapshots::list`, which parses `tmutil
      listlocalsnapshots`.
    - The card shows `tmutil deletelocalsnapshots`; `tmutil` gives no sizes.
  - **Mail attachments:** the guard protects `~/Library/Mail` (§15.6). Mail's *opened*
    copies in its container (`Mail Downloads`) are a separate review rule.
  - **Docker.raw:** review risk with action `report`, so it can never be selected.
- **Simulators** are matched as devices not booted for 90 days. Telling which ones are
  "unavailable" needs `xcrun simctl`; the card names `xcrun simctl delete unavailable`.
- **User and container caches** exclude `com.apple.*` (and `CloudKit` / `Homebrew`, which
  have their own handling).
- **Not selected by default:** Xcode Archives (needed to read crash reports), old
  simulators and old downloads. This is set with the new optional `selected` field.
- **Added:** SwiftUI previews, simulator caches, `~/.npm/_cacache`, Mail Downloads.
- **Deferred to the app inventory (#36–#38):** orphaned app support and orphaned
  preferences. "Orphaned" needs the list of installed apps.
- **Known cautious case:** a sandbox container whose bundle id ends in `.app` looks like
  a bundle to the guard, so its caches are refused (listed, not hit).

### Windows catalog

| Rule | Path | Risk |
|---|---|---|
| User temp | `%TEMP%`, `%LOCALAPPDATA%\Temp` | safe |
| Windows temp | `C:\Windows\Temp` | safe |
| Windows.old | `C:\Windows.old` | review |
| Update cache | `C:\Windows\SoftwareDistribution\Download` | safe |
| Delivery Optimization | `C:\Windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization` | safe |
| Recycle Bin | `$Recycle.Bin` | review |
| Browser caches | Chrome / Edge / Firefox cache dirs | safe |
| Old installers | `~/Downloads/*.{exe,msi}` 90d+ | review |
| Crash dumps | `%LOCALAPPDATA%\CrashDumps` | safe |
| Package manager caches | `%LOCALAPPDATA%\npm-cache`, `pip\Cache`, `~/.cargo\registry`, `~/.nuget\packages` | safe |
| Thumbnail cache | `%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db` | safe |
| Hibernation file | `C:\hiberfil.sys` | report (`powercfg /h off`) |
| Pagefile | `C:\pagefile.sys` | report only |
| WinSxS | `C:\Windows\WinSxS` | report (`DISM /Online /Cleanup-Image /StartComponentCleanup`) |
| Windows Error Reporting | `%LOCALAPPDATA%\Microsoft\Windows\WER` | safe |

**Built (#33).** `crates/analysis/rules/windows.json` has 18 rules, each with a fixture and
an exact byte total in `tests/rules_windows.rs`. Where it differs from the table above:

- **Report-only.** The path guard refuses everything inside `X:\Windows` (§15.4), and
  these locations also need an administrator. So they are report-only, each with the
  command that cleans it properly. Every report-only Windows rule carries a `command`
  (new optional field):
  - Windows temp, Update downloads and `Windows.old`: `cleanmgr /d C:` (Disk Cleanup;
    for Windows.old, Clean up system files);
  - Delivery Optimization: `Delete-DeliveryOptimizationCache -Force`;
  - WinSxS: `DISM /Online /Cleanup-Image /StartComponentCleanup`;
  - `hiberfil.sys`: `powercfg /h off`;
  - `pagefile.sys`: `SystemPropertiesPerformance`;
  - Recycle Bin: `Clear-RecycleBin`. StorageLens never empties the Recycle Bin, as
    with the macOS Trash, because undo relies on it.
- **Deletable rules** all live in the user's profile:
  - temp (7 days);
  - Chrome/Edge `Cache`, `Code Cache`, `GPUCache`, and Firefox `cache2`;
  - old `exe`/`msi` downloads (review);
  - crash dumps, WER reports, `thumbcache_*.db`;
  - npm, pip, Cargo registry, NuGet packages.
- **System32 safety.** Nothing inside `C:\Windows\System32` ever matches. A test runs
  the catalog with `%TEMP%` pointing into System32 (as it does for the SYSTEM account):
  the guard refuses the match.
- **To verify on a real Windows machine:** the `trash` crate's Recycle Bin call passes
  `FOF_WANTNUKEWARNING`, which should make Windows ask before permanently deleting an
  item too large for the Recycle Bin.

### Shared catalog

`node_modules`, `__pycache__`, `.venv`, `target/`, `.next/`, `dist/`, `.gradle/caches`,
duplicate files, files > 1 GB, orphaned app data.

### The Reclaim score

```
score = Σ (bytes × confidence × risk_weight)
risk_weight:  safe = 1.0   review = 0.5   never = 0
```

Surfaced as one headline on the Overview: **"You can safely free ~X GB"**, split into
*safe* and *needs review*. That number is the entire product.

### Built (#31)

The engine is in `storagelens_analysis::rules`, and the catalogs are in
`crates/analysis/rules/*.json`, compiled in.

**Schema additions** (all optional):
- `glob` may be a list.
- `match` is `dir`, `file` or `any`.
- `when_sibling` / `when_child`: names that must sit beside / inside the match (e.g.
  `Cargo.toml` beside `target`, `pyvenv.cfg` inside a venv), so a folder that only shares
  the name never hits.
- `risk` and `confidence` default from `kind`:

  | kind | risk | confidence | selected |
  |---|---|---|---|
  | regenerable | safe | 0.95 | yes |
  | stale | review | 0.8 | yes |
  | duplicate | review | 0.9 | no |
  | leftover | review | 0.6 | no |
  | valuable | keep | 0.5 | no |
  | system | never | 0 | never |

- `action` defaults to `report` for `never`, otherwise `trash`.

**Validation.** A rule is refused, with a reason, if:
- it is `system` without `never`;
- it is `never` with `trash`;
- its confidence is outside 0–1;
- a glob is relative (it must be absolute or start with `**/`);
- a variable (`~`, `%NAME%`) is unset.

The rest of the catalog still loads.

**Globs.** `**`, `*`, `?` and `{a,b}` are supported, matched per component and ignoring
ASCII case.

**Staleness.** It uses the newest file mtime inside the match. With no known time,
nothing is stale.

**Safety, by construction:**
- Every deletable hit passes `safety::Guard`. A deletable rule can never match a system
  location, a protected home folder, or a bundle's insides; refused matches are listed,
  not hit.
- Hits reach deletion only as a `Candidate`, which a `never` or report-only rule cannot
  produce.
- **Consequence for #32/#33:** rules for places the guard protects must be report-only.
  That covers Mail attachments (`~/Library/Mail`) and `$Recycle.Bin`.

**Overlaps and the Reclaim score.**
- A node hit by several rules takes the most cautious verdict.
- A file's verdict is that of its nearest hit, which is what the Cleanliness view shows.
- The score counts every byte once, at that hit, with `keep` weighted 0 like `never`.
  Report-only hits count for nothing.

**Hot reload.** `<catalog dir>/rules/*.json` is read on top of the built-ins; a rule with
the same id replaces the built-in one. A fingerprint of that folder (names, sizes,
mtimes) triggers re-evaluation on the next Cleanliness request. Each evaluation replaces
the scan's `rule_hit` rows.

**Speed.** One walk, with no path strings until a match: about 220 ms over 1M nodes with
the built-in catalog.

**Shared catalog (#31):**
- `node_modules` (stale 90 d, beside `package.json`);
- `__pycache__`;
- `.venv`/`venv` (review, stale 90 d, containing `pyvenv.cfg`);
- Rust `target` (stale 30 d, beside `Cargo.toml`);
- `.next`;
- `dist` (review, stale 90 d, beside `package.json`);
- `~/.gradle/caches`.

Duplicates and "files > 1 GB" are not glob rules; they come with the duplicate finder (M5).

---

## 11. App inventory & unused apps

### Signals

| Signal | macOS | Windows |
|---|---|---|
| Last used | Spotlight `kMDItemLastUsedDate`, `kMDItemUseCount` | Prefetch, UserAssist (`Amcache.hve` and `LastAccessTime` not read — see notes) |
| Installed | `mdfind` on `kMDItemContentType == 'com.apple.application-bundle'` | `HKLM/HKCU\...\Uninstall` (+ `Wow6432Node`) |
| Running | `NSWorkspace.runningApplications` | `EnumProcesses` + image path |
| Autostart | `launchctl list`, `~/Library/LaunchAgents`, `/Library/LaunchDaemons`, `backgrounditems.btm` | `Run` keys, Startup folder, `Task Scheduler` |
| Default handler | LaunchServices | `HKCU\...\FileExts` |
| Data footprint | `~/Library/{Application Support,Caches,Containers,Group Containers}`, prefs plists, saved state | `%LOCALAPPDATA%`, `%APPDATA%`, `%PROGRAMDATA%` |

Windows `LastAccessTime` is **disabled by default** on modern NTFS — never use it as the
sole basis for "unused". Prefer Amcache + Prefetch + Start-menu presence.

### Classification

```
unused        = last_used > 180d AND not running AND not autostart
                AND not a default handler AND not system
likely-unused = last_used 90–180d, no supporting evidence
in-use        = anything else
unknown       = no metadata available  →  never flag
```

### Footprint = app + satellites

Removing an app should show the **total cost**, not just the binary:

```
/Applications/Foo.app                          ← macOS
~/Library/Application Support/<bundleid>
~/Library/Caches/<bundleid>
~/Library/Containers/<bundleid>
~/Library/Group Containers/<group>
~/Library/Preferences/<bundleid>.plist
~/Library/Saved Application State/<bundleid>.savedState

C:\Program Files\Foo\                          ← Windows
%LOCALAPPDATA%\Foo
%APPDATA%\Foo
%PROGRAMDATA%\Foo
```

Satellite matching is a **heuristic** (bundle-id / name substring). Label it
"associated data (estimated)" and always require confirmation.

**macOS implementation notes (#36).** `mdls` prints a `kMDItemLastUsedDate` even for
a bundle Spotlight never saw launch — it falls back to the bundle's file dates — while
`kMDItemUseCount` stays `(null)`. A date without a use count is discarded (→ `unknown`),
or every old install would read as "unused". Satellites match on exact bundle id (plus
the app name for Application Support and Caches); a group container matches when its
name after the team prefix is the bundle id or below it; a path claimed by two apps goes
to neither. Default handlers count only for content types, extensions and
`http`/`https`/`mailto`, not an app's own URL scheme. Login items in
`backgrounditems.btm` are not read.

**Windows implementation notes (#37).** `Amcache.hve` is not read: the system holds
the hive open exclusively (readable only from a shadow copy), and it records first
execution rather than last use. Last use comes from Prefetch file times (admin only) and
UserAssist (`HKCU`, run count + last-run `FILETIME` for shell-launched programs).
`LastAccessTime` is never read. Prefetch names are matched to an app by the `*.exe`
files directly in its install directory; a name two apps share (`update.exe`) is
ignored. Install locations that are a drive root or a well-known parent (`Program
Files`, `%LOCALAPPDATA%`, …) are rejected, falling back to `DisplayIcon`'s directory.
Satellites are `<data root>\<dir name or display name>` and
`<data root>\<publisher>\<…>`, never `<data root>\<publisher>` alone. An app installed
inside another app's directory is counted in both. Classification is the same table
for both platforms (`analysis::apps`).

### Never suggest removing

- Anything in `/System/Applications` or with a `com.apple.` bundle id
- Apps that are the current default handler for a UTI
- Apps running right now
- Anything referenced by an active login item / autostart entry (offer to remove that first)

---

## 12. Duplicate detection

Three stages, each filtering the previous:

1. **Group by exact logical size.** Files with unique sizes cannot be duplicates. This
   alone eliminates ~95% of candidates. (Logical, not `size_alloc`: identical bytes can
   have different allocations — sparse or compressed files — so grouping on `size_alloc`
   would miss real duplicates. `size_alloc` is still what "reclaimable" reports.)
2. **Partial hash** the first 64 KB. Eliminates most remaining false positives.
3. **Full BLAKE3** only for survivors.

Throttle hashing I/O (`IOPOL_THROTTLE` on macOS, `FILE_FLAG_SEQUENTIAL_SCAN` on Windows)
so the disk doesn't stall. Show progress per group, cancellable.

Implementation (#39, `analysis::dupes`): hashing is sequential on the caller's thread,
which sets the I/O throttle (the scanner crates own that syscall). CPU is paced by a
duty cycle — sleep after each 1 MiB chunk so work is ≤ `duty` of wall time; the
default 0.1 measured 8.5% of one core hashing 2 × 256 MiB. Cancel is checked per chunk.
A file whose length no longer matches the scan is skipped as unreadable.

### Keep-one policy

Default suggestion: **keep the newest**, prefer the shallowest path, prefer the one inside
a Documents folder over Downloads over Trash. Never auto-select — show the group and let
me pick.

Skip entirely: files < 1 MB (not worth the I/O), files in system dirs, and anything under
`~/Library` (risky to dedupe app data).

---

## 13. UI & visualization

### Overview

```
┌──────────────────────────────────────────────────────────────┐
│  Macintosh HD · 994 GB                                       │
│  ████████████████████░░░░░░░░░░  712 GB used · 282 free      │
│  [Apps 210] [Photos 180] [Docs 95] [System 120]              │
│  [Caches 45] [Other 62]                                      │
├──────────────────────────────────────────────────────────────┤
│  ✦ You can safely free 68 GB                                 │
│    54 GB safe · 14 GB needs review            [ Review → ]   │
├──────────────────────────────────────────────────────────────┤
│  Biggest consumers           Top cleanup wins                │
│  1. ~/Movies      182 GB     Xcode junk        31 GB         │
│  2. ~/Pictures    141 GB     Old installers    12 GB         │
│  3. ~/Library      88 GB     Caches             9 GB         │
└──────────────────────────────────────────────────────────────┘
```

### Treemap

**Squarified treemap** (Bruls/Huizing/van Wijk) via `d3-hierarchy`, drawn to `<canvas>`.
Not SVG — 1,500 rects in the DOM will crawl.

- **Grouping selector** in the toolbar: `Folder · Type · App · Age · Size · Cleanliness`.
  Swaps the synthetic tree, keeps the same layout engine (§9.6).
- **Level of detail:** draw only nodes with area > 6 px², cap ~1,500 rects, aggregate the
  rest into a single "+N more" tile.
- Labels only when the rect fits the text; otherwise hover tooltip.
- Color by category (stable palette), with toggles for "by age" and "by cleanliness"
  (safe / review / keep).
- Click to zoom with an animated transition; breadcrumb + `Esc` to go up; double-click
  reveals in Finder/Explorer; spacebar previews.
- Sunburst as an alternate view for "who owns the disk" at a glance.
- **Filter chips** above the map: type, size, age, location, cleanliness. Filters compose
  with the grouping — e.g. group by Type, filtered to `Age > 1y` and `Size > 500 MB`.

### Screens

1. **Overview** — gauge, reclaim score, top consumers, quick wins, category breakdown
2. **Map** — treemap/sunburst, grouping selector, breadcrumb, search, filter chips
3. **Large Files** — virtualized table, > 100 MB default, sortable
4. **Apps** — name, size, associated data, last used, remove
5. **Cleanup** — rule cards grouped by kind, checkboxes, running total, Trash button
6. **Duplicates** — hash groups, keep-one picker; a section of Cleanup (#112)
7. ~~**History**~~ — dropped; undo lives on the result panel

### Table performance

1M rows: `react-window` + **keyset pagination**:

```sql
SELECT * FROM entries
WHERE scan_id = ? AND size_alloc < ?
ORDER BY size_alloc DESC
LIMIT 200;
```

Never `OFFSET` at scale. Never load a million rows into JS.

### Search

SQLite FTS5 over `entries.name`, with the tree path reconstructed on demand.

- External-content table (schema v3): tokens only, no second copy of the names.
  `unicode61 remove_diacritics 2` plus the M*, So and Sk categories, so accents fold
  and combining scripts and emoji are searchable. Prefix indexes on 1–3 characters.
- Each scan is indexed in one bulk insert when it finishes, tracked by
  `scan_session.names_indexed`. Per-row triggers cost ~35 s per million entries,
  the bulk insert ~5 s.
- Every term becomes a quoted prefix phrase, ANDed, so user input is never FTS
  syntax. Results rank by size (a folder by its rollup) and then by recency,
  paged with a keyset cursor.
- Measured on 1M names: 1 ms for specific terms, under 50 ms for terms that
  match 17% of all files.

---

## 14. Tauri command surface

```rust
#[tauri::command] async fn list_scan_targets() -> Result<Vec<ScanTarget>, String>; // home + volumes
#[tauri::command] async fn start_scan(roots: Vec<String>) -> Result<u32, String>;
#[tauri::command] async fn cancel_scan(scan_id: u32) -> Result<(), String>;
#[tauri::command] async fn scan_status(scan_id: u32) -> Result<ScanStatus, String>;

#[tauri::command] async fn get_overview(scan_id: u32) -> Result<Overview, String>;
#[tauri::command] async fn get_categories(scan_id: u32) -> Result<Vec<CategoryTotal>, String>;
#[tauri::command] async fn get_grouping(scan_id: u32, by: Grouping, filter: Filter,
    parent_id: Option<i64>, depth: Option<u32>) -> Result<TreeNode, String>;
    // same shape as get_tree; groups that are not folders have negative ids
    // Filter { categories, min_size, older_than_days, within: entry id of a folder }
#[tauri::command] async fn get_children(scan_id: u32, parent_id: u32) -> Result<Vec<Child>, String>;
#[tauri::command] async fn get_tree(scan_id: u32, parent_id: u32, depth: Option<u32>)
    -> Result<TreeNode, String>;   // the treemap's subtree, node-budgeted
#[tauri::command] async fn reveal_entry(scan_id: u32, entry_id: u32) -> Result<(), String>;
#[tauri::command] async fn preview_entry(scan_id: u32, entry_id: u32) -> Result<(), String>;
#[tauri::command] async fn get_files(scan_id: u32, sort: FileSort, reversed: bool, min_size: u64,
    start: u64, count: u32) -> Result<FileRange, String>;   // positional, ≤ 200 rows
#[tauri::command] async fn get_largest(scan_id: u32, min_size: u64, cursor: Option<u64>)
    -> Result<Page<FileRow>, String>;
#[tauri::command] async fn search(scan_id: u32, query: String, cursor: Option<SearchCursor>)
    -> Result<Page<FileRow>, String>;   // 50 per page, biggest then most recent
#[tauri::command] async fn get_ancestors(scan_id: u32, entry_id: u32) -> Result<Vec<Crumb>, String>;

// Cleanup (#34)
#[tauri::command] async fn get_rules(scan_id: i64) -> Result<Cleanup, String>;
    // cards by kind, the reclaim figures, refused count, skipped catalog rules;
    // on macOS a report-only Time Machine snapshots card
#[tauri::command] async fn preview_deletion(scan_id: i64, paths: Vec<String>) -> Result<DeletionPlan, String>;
    // only deletable suggestions of that scan, each once, none inside another
#[tauri::command] async fn execute_deletion(plan_id: String, confirmed: bool) -> Result<DeletionResult, String>;
    // a plan runs once; over 5 GB `confirmed` must be true; emits cleanup://progress
#[tauri::command] async fn cancel_deletion() -> Result<(), String>;
#[tauri::command] async fn undo_deletion(manifest_id: String) -> Result<UndoResult, String>;

#[tauri::command] async fn list_apps(scan_id: u32) -> Result<Vec<AppRow>, String>;
#[tauri::command] async fn find_duplicates(scan_id: u32, min_size: u64)
    -> Result<Vec<DupGroup>, String>;

#[tauri::command] async fn check_full_disk_access() -> Result<bool, String>;
#[tauri::command] async fn open_full_disk_access_settings() -> Result<(), String>;
```

Events (backend → UI):

```
scan://progress   { scan_id, files, bytes, current_path, rate }
scan://done       { scan_id, status, duration_ms }
scan://error      { scan_id, message }
dupes://progress  { scan_id, hashed, total }
```

`reveal_entry` and `preview_entry` take catalog ids, never paths, so the UI can only hand
the OS what the scan saw. Reveal is Finder (`open -R`) or Explorer (`/select,`); preview
is Quick Look on macOS and falls back to reveal on Windows, which has no system preview.

`preview_deletion` is mandatory before `execute_deletion` — the UI can never skip the
dry-run receipt.

---

## 15. Safety model

Personal use means I can be more aggressive than a consumer app — but the cost of a
mistake is my own data, so the mechanics stay.

1. **Trash, never `unlink`.** `trash` crate → macOS Trash / Windows Recycle Bin. Recoverable
   through the OS even if this app is deleted.
2. **Dry-run receipt.** A sheet listing every path, size, and risk badge before anything
   moves. No hidden work.
3. **Preflight re-stat** immediately before deleting: confirm `(size, mtime, inode/file
   index)` still match the scan. If anything changed, skip it and report. *(TOCTOU guard.)*
4. **Path guards:** resolve symlinks/junctions, refuse `/System`, `C:\Windows`,
   `C:\Program Files`, bundle internals, and anything escaping the scanned root.
5. **Manifest + undo.** Write `trash-<timestamp>.json` before moving. Undo restores by
   original path; report anything that couldn't be restored.
6. **Never touch:** Photos library, Music library, Mail store, keychains, `.ssh`,
   Time Machine backups, `pagefile.sys`, system/immutable-flagged files.
7. **Friction on bulk:** anything over 5 GB needs a second confirmation click.
8. **No background deletion.** Ever. Every removal is a click I made, in this session.
9. **Local audit log.** Every action appended to a text log. No network.

**Built (#29).** `safety::guard` and `safety::preflight`.

- **Guard.** A `Guard` is built per cleanup from the resolved scan root and the home
  folder. `Guard::check(path)` refuses:
  - a relative path, or one with a `.`/`..` component (checked before resolving);
  - a path that, once every link and junction is resolved, is not strictly inside the
    root. A symlink target is checked twice: as the link (what the Trash moves) and as
    what it points to, unless it dangles;
  - a path that is, is inside, or *contains* a system location. On macOS: `/System`,
    `/usr`, `/bin`, `/sbin`, `/private/etc`, `/private/var/{db,vm}`, `/cores`,
    `/.DocumentRevisions-V100`, `/.Spotlight-V100`, `/.fseventsd`. On Windows, on any
    drive: `Windows`, `Program Files`, `Program Files (x86)`, `Recovery`,
    `System Volume Information`, `$Recycle.Bin`;
  - the home itself, or any folder containing it;
  - the home's `Pictures` and `Music` folders themselves (files in them may go);
  - `~/.ssh`, `~/.gnupg`, `~/Library/Keychains` and `~/Library/Mail`, including
    everything inside them;
  - §15.6 names, anywhere in the path: `.ssh`, `.gnupg`, `*.keychain[-db]`,
    `*.photoslibrary`, `*.musiclibrary`, `Backups.backupdb`, `*.backupbundle`,
    `Library/{Keychains,Mail}`, `pagefile.sys`, `hiberfil.sys`, `swapfile.sys`;
  - anything below a bundle (`.app`, `.framework`, `.bundle`, `.plugin`, `.kext`, …).
    The bundle itself may go (M5).
- **Case rules.** Deny rules ignore ASCII case on both platforms. Containment is exact on
  macOS (`realpath` returns the on-disk case) and ignores case on Windows.
  `check_resolved` (the rules on an already-resolved path) allocates nothing and makes
  no syscalls.
- **Preflight.** `preflight(path, scanned, stamp)` re-stats the path and compares it with
  the scan. Any of these is a reported `Skip`:
  - the kind changed;
  - the volume or file id changed (replaced);
  - a file's logical size changed;
  - the mtime changed (seconds), for files, folders and links;
  - the item is gone or unreadable;
  - the item is now locked: `UF/SF_IMMUTABLE`, `*_APPEND`, `SF_RESTRICTED` or
    `SF_NOUNLINK` on macOS; `FILE_ATTRIBUTE_SYSTEM` on Windows.
- **Stamps.** `stamp(path)` comes from the platform crate
  (`storagelens_scan_macos::stamp`: `lstat`; `storagelens_scan_windows::stamp`:
  `GetFileInformationByHandle` on a reparse-point handle). It reads exactly what that
  scanner records; each crate's `walk_fixture` test checks this on every fixture entry.
  `core::FileStamp` is the shared shape, and `safety` stays free of `unsafe`.
- **Not covered by the guard.** A folder whose *descendants* include something protected
  (e.g. an ancestor of a `.photoslibrary` elsewhere) cannot be seen from its path alone.
  The plan builder (#31/#34) checks the arena below each folder it offers.
- **Coverage.** `cargo +nightly llvm-cov -p storagelens-safety --branch --summary-only`
  gives 100% of lines and branches.

**Built (#34).** The Cleanup screen puts §15.2–§15.8 in front of the user.

- **Selecting.** Rule cards are grouped by kind (`routes/Cleanup.tsx`,
  `components/RuleCard.tsx`).
  - Only deletable cards have checkboxes, and a report-only card shows its command.
  - Selection starts from each rule's default.
  - The running total counts a selected folder once, even if things inside it are
    selected too (`lib/cleanup.ts`).
- **The receipt** (`components/Receipt.tsx`) is the plan the backend made from the
  selection. It lists every path, size and risk, in manifest order, and a test checks
  that the manifest matches it. Over 5 GB, "Move to Trash" asks twice, and the backend
  refuses the first click too.
- **A plan runs once.** Each item goes through the guard, the preflight and the Trash.
  "Stop" ends the run before the next item; those items are recorded as `cancelled`.
  Moved items are logged in `trash_log` and hidden from the screen until the next scan.
- **The result** lists every item that did not move, with its reason. A missing item is
  reported as skipped ("no longer there"), not refused. Undo puts items back and
  reports each one that it could not.
- **Known limit.** The preflight compares times to the second, as the scan records them.
  A folder changed within the same second as its scan is not caught by time alone.

**Built (#30).** `safety::{trash, manifest, undo, bin}`.

- **`execute`.** `execute(guard, items, manifest_dir, bin, stamp, now)`:
  1. writes `trash-<UTC>.json` with every item `planned`. If that write fails, it returns
     the error and nothing has moved;
  2. per item, immediately before its move, runs the guard and then the preflight.
     Failures are recorded as `refused` or `skipped` with the reason;
  3. moves the item to the Trash (`failed` if the move errors);
  4. rewrites the manifest with every outcome.
- **Manifest writes.** Both writes are atomic: temp file, `sync_all`, rename. A failed
  final write is returned as a warning: the moves happened, and undo works from the first
  write.
- **Crash recovery.** Each item records its scan stamp. Undo finds trashed items by
  volume + file id, which a move into the Trash preserves. So items moved before a crash
  are restored even though the manifest still says `planned`.
- **`undo`.** `undo(manifest, bin, now)` restores each trashed item, and each `planned`
  item that is gone, to its original path. It recreates missing parent folders and never
  overwrites anything.
  - Per item it reports one of `Restored`, `AlreadyRestored`, `NotTrashed`, `NotInTrash`,
    `Occupied` or `Failed(reason)`.
  - Restored items are stamped in the manifest, so a second undo moves nothing.
- **macOS Trash.** `bin::SystemBin`:
  - moves items with `NSFileManager.trashItemAtURL` (no Automation prompt, no Finder
    "Put Back");
  - finds them in one pass over `~/.Trash` and `<volume>/.Trashes/<uid>`, by inode;
  - restores by rename.
  - Reading `~/.Trash` needs Full Disk Access; without it, undo reports that the Trash
    could not be searched.
- **Windows Recycle Bin.** It uses the `trash` crate's listing (original path + deletion
  time) and `restore_all`.
- **Tests.** They use a Trash folder inside a temp dir. The one real-Trash round trip is
  an ignored test that you run by hand:
  `cargo test -p storagelens-safety --test system_bin -- --ignored`.
- **Coverage.** That real-Trash call is the only code excluded from coverage.
- **Not built yet.** The `trash_log` table, the audit log (§15.9), and the Tauri commands
  are wired in with the Cleanup screen (#34).

---

## 16. Build schedule

Scanner first. If the numbers are wrong, nothing else matters.

| # | Milestone | Deliverable | Est. |
|---|---|---|---|
| **M0** | Core + CLI | Cargo workspace, `Entry`, schema, migrations, `scanbench` | 3 days |
| **M1** | macOS scanner | `getattrlistbulk` FFI, buffer parser, hard-link dedupe, firmlink skips, ≤2% vs `du -skx` | 1 wk |
| **M2** | Windows scanner | Bulk directory-query walk, junction skips, long paths, ≤2% vs `Get-ChildItem` | 1 wk |
| **M3** | Catalog + treemap | Tree arena, rollups, Tauri shell, React treemap, breadcrumb, search | 1.5 wk |
| **M3b** | Grouping views | Shared category table, tags, synthetic trees, grouping selector, filter chips | 4 days |
| **M4** | Cleanup | Rules engine, both catalogs, Cleanup UI, trash + undo + manifest | 1.5 wk |
| **M5** | Apps | Inventory, footprint, last-used, removal flow — both platforms | 1 wk |
| **M6** | Duplicates | Size grouping, blake3 verify, keep-one UI, throttled I/O | 4 days |

**≈ 8.5 weeks total.** M1 and M2 can overlap once the trait is stable.

### Week 1, concretely

- Day 1: `cargo new` workspace, `core` + `catalog` crates, `Entry`, schema, migrations.
- Day 2: `getattrlistbulk` FFI, open a directory, get raw bytes back.
- Day 3: buffer parser + `stat(2)` cross-check test.
- Day 4: `scanbench` walks `~/Documents`, prints count/size/time.
- Day 5: compare against `du -skx`, fix drift to ≤2%.

If week 1 ends with `scanbench` agreeing with `du` within 2%, the project is real.

---

## 17. Testing & accuracy gates

Accuracy is the product, so gate on it **before** building UI.

### `scanbench` CLI

```
$ scanbench ~/Documents
path            /Users/me/Documents
files           184,203
dirs            12,884
allocated       48.21 GB
logical         51.07 GB
skipped         37 dirs (permission denied)
elapsed         4.31 s
peak rss        212 MB
```

`allocated` and `logical` use binary units: whole numbers at 100 and above, two
decimals below (`48.21 GB`, `212 MB`). Counts are thousands-grouped. `skipped` lists
the commonest reason first.

```text
scanbench <PATH> [--json] [--budget] [--db PATH]
scanbench --fixtures [--json] [--budget]
```

- `--fixtures` scans the generated tree and compares every count and byte total with
  its manifest. `--db PATH` keeps the catalog instead of using a temporary one.
- Every run persists through the real `Catalog` + `EntryWriter`, then asserts that the
  stored row count equals the number of entries the walker produced.
- `--json` replaces the table with the document below, and `--budget` appends a
  `budget` line (and exits `1` on a miss).
- Exit codes: `0` ok, `1` over budget, `2` scan, catalog, or fixture-validation
  failure. A fixture mismatch wins over an over-budget result.

`--json` is stable within `schema_version`. New keys may be added; existing keys keep
their name, type, and meaning. `fixture` and `budget` appear only when requested.

```json
{
  "schema_version": 1,
  "path": "/Users/me/Documents",
  "status": "partial",
  "files": 184203,
  "dirs": 12884,
  "links": 120,
  "entries": 197327,
  "allocated_bytes": 51765093335,
  "logical_bytes": 54835994951,
  "skipped": [
    { "path": "/Users/me/Documents/private", "reason": "permission_denied" }
  ],
  "skipped_by_reason": { "permission_denied": 37 },
  "elapsed_ms": 4310,
  "peak_rss_bytes": 222298112,
  "db": "/tmp/scanbench/scanbench.db",
  "fixture": { "ok": true, "mismatches": [] },
  "budget": {
    "ok": true,
    "checks": [
      { "name": "peak_rss", "limit": 419430400, "actual": 222298112, "ok": true }
    ]
  }
}
```

`elapsed_ms` covers the walk only, not catalog open or session bookkeeping.
`peak_rss_bytes` is the highest sampled resident set size (`0` if the OS refuses to
report it). `skipped_by_reason` uses the `SkipReason` names from `crates/core`.

The elapsed-time budget applies only to scans large enough to compare: `≤ 90 s` at
`≥ 1M` files and `≤ 240 s` at `≥ 3M`. Peak RSS (`≤ 400 MB`) is checked on every run.

### Cross-check harness

| Platform | Compare against | Tolerance |
|---|---|---|
| macOS | `du -skx`, `df -k` | ≤ 2% |
| Windows | `Get-ChildItem -Recurse \| Measure-Object Length -Sum` | ≤ 2% |

Exclusions from the comparison: snapshots, purgeable, clones/reflinks, pagefile, hiberfil,
and `System Volume Information`. Any drift outside that is a bug.

### Fixture trees

Generate a synthetic tree containing:

- hard links (3 names → 1 inode)
- symlink loops (`a → b`, `b → a`)
- Windows junctions pointing at parents
- sparse files (4 GB logical, 8 KB allocated)
- unicode names, emoji, RTL text
- 255-byte names, paths > 1,024 chars
- permission-denied directories
- empty directories
- deeply nested (200 levels)

Assert exact counts and exact byte totals. This suite is the regression net for both
scanners.

### Safety tests (100% coverage)

- symlink escape (link points outside the scanned root)
- TOCTOU (file replaced between scan and delete)
- path traversal (`../../etc/passwd`)
- refusal to delete inside an app bundle
- manifest write failure → abort before moving anything
- undo with a missing Trash item → reports, doesn't crash

**Built (#35).** The safety suite covers every case above, and also:
- preflight skip reporting;
- a cancelled deletion;
- the no-allocation fast path.

`crates/safety` has 100% line and branch coverage. Excluded are only the two OS calls no
test may make: the real Trash move and the real volume lookup.

`scripts/safety-mutants.py` removes each of 21 guards in turn (traversal, root
containment, system, protected and never-touch paths, bundles, both symlink checks, the
five preflight comparisons, manifest-first, guard and preflight before each move, stop,
and undo's two no-overwrite checks). It requires a failing test for every one.

No test touches a real path:
- filesystem tests work inside their own temp dir;
- path rules are tested as strings;
- Trash folders are found through an injected volume lookup.

There is no CI (#64), so the suite runs in the local gate (`cargo test`). The mutation
script runs after any change to `crates/safety`.

### Performance checks

Manual, not automated (there is no CI, #64): before a release, run `scanbench --budget`
against a fixed 200k-file tree and the ignored 1M-node arena benchmark, and treat a
>20% regression as a bug.

### Category invariants

These are cheap to assert and catch real bugs:

- `Σ category bytes == total scanned bytes` (nothing unclassified, nothing double-counted)
- `Σ type-grouping tree bytes == total scanned bytes`
- Every grouping view (`Folder`, `Type`, `App`, `Age`, `Size`, `Cleanliness`) produces the
  same root total. If one view disagrees, the synthetic-tree builder is wrong.
- Fixture tree has a known file per category; assert exact assignment for all ~400
  extensions in the shared table.
- `Other` is always present and its byte total is reported, never hidden.

---

## 18. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Sizes wrong → tool is useless | Critical | `scanbench` accuracy gate before UI; always show allocated vs logical |
| Delete something important | Critical | Trash-only, dry-run receipt, manifest, undo, preflight re-stat, bulk friction |
| `getattrlistbulk` buffer offsets wrong | High | `stat(2)` cross-check test on every entry in the fixture tree |
| Rust slower to write than expected | Medium | Fallback to Go core — schema, rules, and UI unchanged |
| Windows MFT work is deep | Medium | Skip in v1; the bulk directory query (§7.1) is fast enough |
| macOS FDA confusion | Medium | Poll + deep link + "scan anyway" degraded mode with visible gaps |
| APFS/NTFS clone overcounting | Low | Documented; personal tool |
| Two platforms drift | Low | One schema, one rules catalog, one UI; only `ScanKit` is per-platform |

---

## 19. The MVP cut

For personal use, the 80/20 is:

**M0 → M1 → M3** = scanner + treemap on Mac. **~3 weeks.** Immediately useful.

**M3b** (grouping by type) is only ~4 extra days and roughly doubles how useful the map is,
because "how much of my disk is video?" is the question people actually ask. Do it early.

Then **M4** (cleanup rules) — that's where the actual gigabytes come from.

**M5 (apps) and M6 (duplicates) are the least value per hour.** Do them last, or never.
Duplicates in particular are expensive (full-file hashing) for usually modest returns on a
modern SSD where the real space is in caches, old installers, and dev artifacts.

### Ship criteria for v1

- [ ] `scanbench` within 2% of `du` on the fixture tree
- [ ] Scan of 1M files completes in under 90 s cold
- [ ] Treemap stays at 60 fps while zooming
- [ ] Category totals sum exactly to the scan total
- [ ] All six grouping views agree on the root total
- [ ] Cleanup finds ≥ 3 categories with real reclaim on my machine
- [ ] Trash + undo round-trips perfectly
- [ ] Nothing under `/System` or `C:\Windows` is ever deletable
