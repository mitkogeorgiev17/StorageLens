/**
 * The typed IPC surface: one function per Tauri command (PLAN §14), one listener
 * per event. The types here are hand-kept twins of `src-tauri/src/dto.rs` and
 * `events.rs`; change both sides together.
 *
 * Commands reject with the backend's `String` error. {@link call} turns that
 * into an `ApiError`, so callers never handle `unknown`.
 *
 * Byte counts are JSON numbers: exact up to 2^53 bytes (8 PiB).
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { ScanProgress, ScanSummary } from "../store";

// --- Shared value types -------------------------------------------------------

export type ScanStatus = "running" | "complete" | "cancelled" | "partial";

export type EntryKind = "File" | "Dir" | "Symlink" | "Other";

/** Mirrors `core::categories::Category` (kebab-case). */
export type Category =
  | "images"
  | "video"
  | "audio"
  | "documents"
  | "spreadsheets"
  | "presentations"
  | "archives"
  | "code"
  | "data"
  | "fonts"
  | "three-d"
  | "apps"
  | "disk-images"
  | "other";

export interface ScanStatusDto {
  readonly scanId: number;
  readonly status: ScanStatus;
  readonly root: string | null;
  readonly files: number;
  readonly dirs: number;
  readonly bytesAlloc: number;
  readonly elapsedMs: number;
  readonly skippedDirs: number;
  readonly elevated: boolean | null;
}

export interface VolumeDto {
  readonly name: string;
  readonly mountPoint: string;
  readonly total: number;
  /** Includes `purgeable`. */
  readonly available: number;
  /** `total − available`. */
  readonly used: number;
  /** macOS purgeable space, part of `available`; 0 where not measured. */
  readonly purgeable: number;
}

export interface CategoryDto {
  readonly category: Category;
  readonly label: string;
  readonly bytesAlloc: number;
  readonly fileCount: number;
}

export interface ChildDto {
  readonly id: number;
  readonly name: string;
  readonly kind: EntryKind;
  readonly category: Category;
  /** Own bytes for a file, subtree total for a directory. */
  readonly sizeAlloc: number;
  readonly fileCount: number;
  readonly hasChildren: boolean;
  readonly hardlinkDup: boolean;
}

export interface ChildrenDto {
  readonly items: readonly ChildDto[];
  /** Children beyond the limit, drawn as one "smaller items" block. */
  readonly remainingCount: number;
  readonly remainingBytes: number;
}

/**
 * A directory's subtree for the treemap. Sizes add up at every level: loaded
 * children plus `remainingBytes`. `children: null` means not expanded (the
 * backend's node budget ran out); `hasChildren` still says whether it has any.
 */
export interface TreeNodeDto {
  readonly id: number;
  readonly name: string;
  readonly kind: EntryKind;
  readonly category: Category;
  readonly sizeAlloc: number;
  readonly fileCount: number;
  readonly hasChildren: boolean;
  readonly children: readonly TreeNodeDto[] | null;
  readonly remainingCount: number;
  readonly remainingBytes: number;
}

export interface OverviewDto {
  readonly scan: ScanStatusDto;
  readonly rootId: number;
  readonly rootName: string;
  readonly volume: VolumeDto | null;
  readonly categories: readonly CategoryDto[];
  readonly biggest: readonly ChildDto[];
}

export interface FileRowDto {
  readonly id: number;
  readonly name: string;
  readonly path: string;
  readonly category: Category;
  readonly sizeAlloc: number;
  /** Apparent size: show only labelled as such (AGENTS.md invariant 1). */
  readonly sizeLogical: number;
  readonly modified: number | null;
}

export interface SizeCursor {
  readonly sizeAlloc: number;
  readonly id: number;
}

export interface Page<T, C> {
  readonly items: readonly T[];
  readonly nextCursor: C | null;
}

export type Grouping = "folder" | "type" | "app" | "age" | "size" | "cleanliness";

export interface Filter {
  readonly categories?: readonly Category[];
  readonly minSize?: number;
  readonly olderThanDays?: number;
}

export interface SyntheticNodeDto {
  readonly label: string;
  readonly sizeAlloc: number;
  readonly fileCount: number;
  readonly children: readonly SyntheticNodeDto[];
}

export interface RuleGroupDto {
  readonly ruleId: string;
  readonly title: string;
  readonly risk: string;
  readonly bytes: number;
  readonly paths: readonly string[];
}

export interface DeletionPlanDto {
  readonly planId: string;
  readonly paths: readonly string[];
  readonly createdAt: number;
}

export interface DeletionResultDto {
  readonly manifestId: string;
  readonly trashed: readonly string[];
  readonly skipped: readonly string[];
  readonly bytesFreed: number;
}

export interface AppRowDto {
  readonly name: string;
  readonly path: string;
  readonly bytesAlloc: number;
  readonly lastUsed: number | null;
}

export interface DupGroupDto {
  readonly hash: string;
  readonly sizeAlloc: number;
  readonly paths: readonly string[];
}

export type DiskAccess = "granted" | "denied" | "unknown" | "not-applicable";

// --- Errors ------------------------------------------------------------------------

/** A command failed; `message` is the backend's explanation. */
export class ApiError extends Error {
  readonly command: string;

  constructor(command: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.command = command;
  }
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error: unknown) {
    const message =
      typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
    throw new ApiError(command, message);
  }
}

// --- Commands ----------------------------------------------------------------------

export const api = {
  startScan: (root: string) => call<number>("start_scan", { roots: [root] }),
  cancelScan: (scanId: number) => call<void>("cancel_scan", { scanId }),
  scanStatus: (scanId: number) => call<ScanStatusDto>("scan_status", { scanId }),
  latestScan: () => call<ScanStatusDto | null>("latest_scan"),

  getOverview: (scanId: number) => call<OverviewDto>("get_overview", { scanId }),
  getCategories: (scanId: number) => call<readonly CategoryDto[]>("get_categories", { scanId }),
  getGrouping: (scanId: number, by: Grouping, filter?: Filter) =>
    call<SyntheticNodeDto>("get_grouping", { scanId, by, filter: filter ?? null }),
  getChildren: (scanId: number, parentId: number, limit?: number) =>
    call<ChildrenDto>("get_children", { scanId, parentId, limit: limit ?? null }),
  getTree: (scanId: number, parentId: number, depth?: number) =>
    call<TreeNodeDto>("get_tree", { scanId, parentId, depth: depth ?? null }),
  /** Show the entry selected in Finder / Explorer. */
  revealEntry: (scanId: number, entryId: number) =>
    call<void>("reveal_entry", { scanId, entryId }),
  /** Quick Look on macOS; on Windows, reveal in Explorer (no system preview). */
  previewEntry: (scanId: number, entryId: number) =>
    call<void>("preview_entry", { scanId, entryId }),
  getLargest: (scanId: number, minSize: number, cursor?: SizeCursor) =>
    call<Page<FileRowDto, SizeCursor>>("get_largest", {
      scanId,
      minSize,
      cursor: cursor ?? null,
    }),
  search: (scanId: number, query: string, cursor?: SizeCursor) =>
    call<Page<FileRowDto, SizeCursor>>("search", { scanId, query, cursor: cursor ?? null }),

  getRules: (scanId: number) => call<readonly RuleGroupDto[]>("get_rules", { scanId }),
  previewDeletion: (paths: readonly string[]) =>
    call<DeletionPlanDto>("preview_deletion", { paths }),
  /** Requires a `planId` from {@link api.previewDeletion}; the backend enforces it. */
  executeDeletion: (planId: string) => call<DeletionResultDto>("execute_deletion", { planId }),
  undoDeletion: (manifestId: string) => call<void>("undo_deletion", { manifestId }),

  listApps: (scanId: number) => call<readonly AppRowDto[]>("list_apps", { scanId }),
  findDuplicates: (scanId: number, minSize: number) =>
    call<readonly DupGroupDto[]>("find_duplicates", { scanId, minSize }),

  checkFullDiskAccess: () => call<DiskAccess>("check_full_disk_access"),
  openFullDiskAccessSettings: () => call<void>("open_full_disk_access_settings"),
} as const;

// --- Events ------------------------------------------------------------------------

export interface ScanProgressEvent {
  readonly scanId: number;
  readonly files: number;
  readonly dirs: number;
  readonly bytesAlloc: number;
  readonly elapsedMs: number;
  readonly currentPath: string | null;
  /** Files per second. */
  readonly rate: number;
}

export interface ScanDoneEvent {
  readonly scanId: number;
  readonly status: ScanStatus;
  readonly durationMs: number;
}

export interface ScanErrorEvent {
  readonly scanId: number;
  readonly message: string;
}

export interface DupesProgressEvent {
  readonly scanId: number;
  readonly hashed: number;
  readonly total: number;
}

export const EVENTS = {
  scanProgress: "scan://progress",
  scanDone: "scan://done",
  scanError: "scan://error",
  dupesProgress: "dupes://progress",
} as const;

/** Map a status DTO to the store's summary. A still-running scan has none. */
export function toSummary(dto: ScanStatusDto): ScanSummary | null {
  if (dto.status === "running") {
    return null;
  }
  return {
    scanId: dto.scanId,
    root: dto.root ?? "",
    status: dto.status,
    files: dto.files,
    dirs: dto.dirs,
    bytesAlloc: dto.bytesAlloc,
    elapsedMs: dto.elapsedMs,
    skippedDirs: dto.skippedDirs,
    elevated: dto.elevated,
  };
}

export function toProgress(event: ScanProgressEvent): ScanProgress {
  return {
    files: event.files,
    dirs: event.dirs,
    bytesAlloc: event.bytesAlloc,
    elapsedMs: event.elapsedMs,
    currentPath: event.currentPath,
  };
}

/** The store actions the event bridge drives. */
export interface ScanSink {
  scanProgressed(progress: ScanProgress): void;
  scanFinished(summary: ScanSummary): void;
  scanFailed(message: string): void;
}

/**
 * Forward scan events into the store. On `scan://done` the final status is
 * fetched once, so the store holds the backend's numbers, not the last
 * (throttled) progress snapshot. Returns a function that stops listening.
 */
export async function bridgeScanEvents(sink: ScanSink): Promise<UnlistenFn> {
  const unlisteners = await Promise.all([
    listen<ScanProgressEvent>(EVENTS.scanProgress, (event) =>
      sink.scanProgressed(toProgress(event.payload)),
    ),
    listen<ScanDoneEvent>(EVENTS.scanDone, (event) => {
      api
        .scanStatus(event.payload.scanId)
        .then((dto) => {
          const summary = toSummary(dto);
          if (summary !== null) {
            sink.scanFinished(summary);
          }
        })
        .catch((error: unknown) =>
          sink.scanFailed(error instanceof Error ? error.message : String(error)),
        );
    }),
    listen<ScanErrorEvent>(EVENTS.scanError, (event) => sink.scanFailed(event.payload.message)),
  ]);
  return () => {
    for (const unlisten of unlisteners) {
      unlisten();
    }
  };
}
