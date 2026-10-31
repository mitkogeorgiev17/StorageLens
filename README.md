# StorageLens

A personal storage mapper for macOS and Windows. Indexes every file on the machine,
visualizes it as a zoomable treemap, groups it by type / app / age / cleanliness, and
recommends safely deletable junk — with Trash + undo.

**Private, offline, for one person.** No accounts, no telemetry, no network access.

- **[`PLAN.md`](./PLAN.md)** — full design, architecture, data model, rules catalog,
  build schedule.
- **[`AGENTS.md`](./AGENTS.md)** — conventions and invariants for contributors (human or
  otherwise).

## Status

M0 — Foundation. Workspace, crates, Tauri shell, and CI are in place; domain types and
scanners follow. See `PLAN.md` §16 and the GitHub milestones.

## Stack

| Layer | Choice |
|---|---|
| Shell | Tauri 2 |
| Core | Rust (both scanners, analysis, safety) |
| UI | React 19 + TypeScript + Vite |
| Storage | SQLite (bundled `rusqlite`) |
| Rendering | `d3-hierarchy` squarified treemap → `<canvas>` |

## Targets

| Metric | Budget |
|---|---|
| 1M files, cold cache | < 90 s |
| 1M files, warm cache | < 20 s |
| Peak RSS | < 400 MB |
| Size accuracy vs `du` | ≤ 2% |
