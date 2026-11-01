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
  | "caches"
  | "app-data"
  | "system"
  | "developer"
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

export interface ScanTargetDto {
  readonly kind: "volume" | "home";
  readonly label: string;
  /** Absolute path for {@link api.startScan}. */
  readonly path: string;
  readonly total: number | null;
  readonly available: number | null;
  readonly removable: boolean;
  /** The disk the OS booted from: the recommended full scan. */
  readonly startup: boolean;
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
  /** `File` everywhere but search, which also finds folders. */
  readonly kind: EntryKind;
  readonly path: string;
  readonly category: Category;
  readonly sizeAlloc: number;
  /** Apparent size: show only labelled as such (AGENTS.md invariant 1). */
  readonly sizeLogical: number;
  readonly modified: number | null;
}

/** Natural directions: size largest first, modified oldest first, type by category. */
export type FileSort = "size" | "modified" | "type";

export interface FileRangeDto {
  /** Rows in the whole listing (after the size filter). */
  readonly total: number;
  readonly start: number;
  readonly items: readonly FileRowDto[];
}

/** Keyset cursor for search: biggest, then most recent, then id. */
export interface SearchCursor {
  readonly sizeAlloc: number;
  readonly modified: number;
  readonly id: number;
}

export interface CrumbDto {
  readonly id: number;
  readonly name: string;
}

export interface SizeCursor {
  readonly sizeAlloc: number;
  readonly id: number;
}

export interface Page<T, C> {
  readonly items: readonly T[];
  readonly nextCursor: C | null;
}

export type Grouping =
  "folder" | "type" | "app" | "age" | "size" | "cleanliness";

export interface Filter {
  readonly categories?: readonly Category[];
  readonly minSize?: number;
  readonly olderThanDays?: number;
  /** Only files inside this folder (an entry id). */
  readonly within?: number;
}

/** Something that starts at login (#114). */
export interface StartupItemDto {
  readonly id: string;
  readonly name: string;
  /** What runs: a command line, program path or shortcut. */
  readonly command: string;
  /** Where it is registered. */
  readonly source: string;
  readonly enabled: boolean;
  /** `false`: switching it needs admin rights or the system settings. */
  readonly canToggle: boolean;
}

/** What a force quit did (#113). */
export interface QuitDto {
  readonly stopped: number;
  /** Processes that refused: another user's, elevated, or protected. */
  readonly failed: number;
}

/** What the Cleanup screen shows for one scan (#34). */
export interface CleanupDto {
  readonly scanId: number;
  /** By kind, biggest first within a kind. */
  readonly groups: readonly RuleGroupDto[];
  /** Matches the path guard refused; never offered. */
  readonly refused: number;
  /** Σ bytes × confidence over safe suggestions, each byte once (PLAN §10). */
  readonly reclaimSafe: number;
  /** The same over suggestions that need review. */
  readonly reclaimReview: number;
  readonly problems: readonly string[];
}

export type RuleKind =
  "regenerable" | "stale" | "duplicate" | "leftover" | "valuable" | "system";
export type RiskLevel = "safe" | "review" | "keep" | "never";

/** One rule card. */
export interface RuleGroupDto {
  readonly ruleId: string;
  readonly title: string;
  readonly kind: RuleKind;
  readonly risk: RiskLevel;
  readonly explain: string;
  /** For report-only cards: what to run instead. */
  readonly command: string | null;
  readonly deletable: boolean;
  readonly bytes: number;
  readonly items: readonly RuleItemDto[];
}

export interface RuleItemDto {
  /** Absolute path, or a snapshot's name. */
  readonly path: string;
  readonly entryId: number | null;
  readonly bytes: number;
  readonly selected: boolean;
}

/** The dry-run receipt: exactly what will move, in manifest order. */
export interface DeletionPlanDto {
  readonly planId: string;
  readonly scanId: number;
  readonly items: readonly PlanItemDto[];
  readonly totalBytes: number;
  /** Over 5 GB: execution needs a second confirmation. */
  readonly needsConfirmation: boolean;
  readonly createdAt: number;
}

export interface PlanItemDto {
  readonly path: string;
  readonly bytes: number;
  readonly ruleId: string;
  readonly title: string;
  readonly risk: RiskLevel;
}

export type ItemStatus =
  "trashed" | "refused" | "skipped" | "failed" | "cancelled";

export interface DeletionResultDto {
  /** Pass to {@link api.undoDeletion}. */
  readonly manifestId: string;
  readonly items: readonly ResultItemDto[];
  readonly trashed: number;
  readonly trashedBytes: number;
  readonly warning: string | null;
}

export interface ResultItemDto {
  readonly path: string;
  readonly bytes: number;
  readonly status: ItemStatus;
  readonly reason: string | null;
}

export type UndoOutcome =
  | "restored"
  | "already-restored"
  | "not-trashed"
  | "not-in-trash"
  | "occupied"
  | "failed";

export interface UndoResultDto {
  readonly items: readonly {
    readonly path: string;
    readonly result: UndoOutcome;
    readonly reason: string | null;
  }[];
  readonly restored: number;
  readonly warning: string | null;
}

export interface CleanupProgressEvent {
  readonly planId: string;
  readonly done: number;
  readonly total: number;
}

export type AppStatus = "in_use" | "likely_unused" | "unused" | "unknown";

export interface SatelliteDto {
  readonly path: string;
  readonly bytes: number;
}

/** One installed app. System apps never reach the UI. */
export interface AppRowDto {
  readonly id: string;
  readonly name: string;
  readonly version: string | null;
  /** Empty when Windows recorded no install directory: not removable. */
  readonly path: string;
  readonly appBytes: number;
  /** Associated data: a heuristic, always labelled "estimated". */
  readonly satelliteBytes: number;
  readonly totalBytes: number;
  readonly lastUsed: number | null;
  readonly useCount: number | null;
  readonly status: AppStatus;
  readonly running: boolean;
  readonly autostart: boolean;
  readonly defaultHandler: boolean;
  /** The app has its own uninstaller (#115); see {@link api.uninstallApp}. */
  readonly hasUninstaller: boolean;
  readonly satellites: readonly SatelliteDto[];
}

/** One copy of a duplicated file. */
export interface DupFileDto {
  readonly path: string;
  readonly sizeAlloc: number;
  /** Unix seconds, as scanned. */
  readonly modified: number | null;
}

/** Byte-identical files; always two or more still on disk. */
export interface DupGroupDto {
  /** BLAKE3, hex. */
  readonly hash: string;
  readonly sizeLogical: number;
  /** Freed by keeping one copy. */
  readonly reclaimable: number;
  readonly files: readonly DupFileDto[];
}

export interface DupesDto {
  readonly scanId: number;
  /** Largest reclaimable first. */
  readonly groups: readonly DupGroupDto[];
  readonly unreadable: number;
  /** Stopped early: only finished groups are listed. */
  readonly cancelled: boolean;
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

async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error: unknown) {
    const message =
      typeof error === "string"
        ? error
        : error instanceof Error
          ? error.message
          : String(error);
    throw new ApiError(command, message);
  }
}

// --- Commands ----------------------------------------------------------------------

export const api = {
  listScanTargets: () => call<readonly ScanTargetDto[]>("list_scan_targets"),
  startScan: (root: string) => call<number>("start_scan", { roots: [root] }),
  cancelScan: (scanId: number) => call<void>("cancel_scan", { scanId }),
  scanStatus: (scanId: number) =>
    call<ScanStatusDto>("scan_status", { scanId }),
  latestScan: () => call<ScanStatusDto | null>("latest_scan"),

  getOverview: (scanId: number) =>
    call<OverviewDto>("get_overview", { scanId }),
  getCategories: (scanId: number) =>
    call<readonly CategoryDto[]>("get_categories", { scanId }),
  /**
   * One of the six grouping views (PLAN §9.6), as the same tree shape as
   * {@link api.getTree}: `parentId` zooms into a group (negative ids are
   * synthetic groups, positive ones folders or files).
   */
  getGrouping: (
    scanId: number,
    by: Grouping,
    filter?: Filter,
    parentId?: number,
    depth?: number,
  ) =>
    call<TreeNodeDto>("get_grouping", {
      scanId,
      by,
      filter: filter ?? null,
      parentId: parentId ?? null,
      depth: depth ?? null,
    }),
  getChildren: (scanId: number, parentId: number, limit?: number) =>
    call<ChildrenDto>("get_children", {
      scanId,
      parentId,
      limit: limit ?? null,
    }),
  getTree: (scanId: number, parentId: number, depth?: number) =>
    call<TreeNodeDto>("get_tree", { scanId, parentId, depth: depth ?? null }),
  /** Show the entry selected in Finder / Explorer. */
  revealEntry: (scanId: number, entryId: number) =>
    call<void>("reveal_entry", { scanId, entryId }),
  /** Quick Look on macOS; on Windows, reveal in Explorer (no system preview). */
  previewEntry: (scanId: number, entryId: number) =>
    call<void>("preview_entry", { scanId, entryId }),
  /** Rows `start ..` of every file, positional; at most 200 per call. */
  getFiles: (
    scanId: number,
    sort: FileSort,
    reversed: boolean,
    minSize: number,
    start: number,
    count: number,
  ) =>
    call<FileRangeDto>("get_files", {
      scanId,
      sort,
      reversed,
      minSize,
      start,
      count,
    }),
  getLargest: (scanId: number, minSize: number, cursor?: SizeCursor) =>
    call<Page<FileRowDto, SizeCursor>>("get_largest", {
      scanId,
      minSize,
      cursor: cursor ?? null,
    }),
  /** Files and folders whose name matches, biggest first; 50 per page. */
  search: (scanId: number, query: string, cursor?: SearchCursor) =>
    call<Page<FileRowDto, SearchCursor>>("search", {
      scanId,
      query,
      cursor: cursor ?? null,
    }),
  /** The folders from the scan root down to the entry, inclusive. */
  getAncestors: (scanId: number, entryId: number) =>
    call<readonly CrumbDto[]>("get_ancestors", { scanId, entryId }),

  getRules: (scanId: number) => call<CleanupDto>("get_rules", { scanId }),
  previewDeletion: (scanId: number, paths: readonly string[]) =>
    call<DeletionPlanDto>("preview_deletion", { scanId, paths }),
  /**
   * Requires a `planId` from {@link api.previewDeletion}; the backend enforces
   * it. `confirmed` is the second click a plan over 5 GB needs.
   */
  executeDeletion: (planId: string, confirmed: boolean) =>
    call<DeletionResultDto>("execute_deletion", { planId, confirmed }),
  /** Stop an executing deletion before its next item. */
  cancelDeletion: () => call<void>("cancel_deletion"),
  undoDeletion: (manifestId: string) =>
    call<UndoResultDto>("undo_deletion", { manifestId }),

  /** Installed apps; `refresh` re-reads the machine, which takes seconds. */
  listApps: (refresh: boolean) =>
    call<readonly AppRowDto[]>("list_apps", { refresh }),
  /** Everything that starts at login (#114). */
  listStartup: () => call<readonly StartupItemDto[]>("list_startup"),
  /** Switch a startup item for future logins; returns the refreshed list. */
  setStartup: (id: string, enabled: boolean) =>
    call<readonly StartupItemDto[]>("set_startup", { id, enabled }),
  /** The system's own startup settings, for items this app cannot switch. */
  openStartupSettings: () => call<void>("open_startup_settings"),
  /**
   * Start a listed app's own uninstaller (#115): on Windows what "Add or
   * remove apps" runs, on macOS a vendor uninstaller. Resolves once started.
   */
  uninstallApp: (id: string) => call<void>("uninstall_app", { id }),
  /** Force quit a listed app's processes (#113); unsaved work is lost. */
  forceQuitApp: (path: string) => call<QuitDto>("force_quit_app", { path }),
  /**
   * Receipt for removing an app (and, with `includeData`, its associated
   * data). Executed by {@link api.executeDeletion} like any cleanup plan.
   */
  previewAppRemoval: (
    scanId: number,
    path: string,
    includeData: boolean,
    allowRunning: boolean,
  ) =>
    call<DeletionPlanDto>("preview_app_removal", {
      scanId,
      path,
      includeData,
      allowRunning,
    }),
  /**
   * Byte-identical files of a scan. Reuses the last search unless `refresh`;
   * a fresh search hashes in the background and can take minutes.
   */
  findDuplicates: (scanId: number, refresh: boolean) =>
    call<DupesDto>("find_duplicates", { scanId, refresh }),
  cancelDuplicates: () => call<void>("cancel_duplicates"),
  /**
   * Receipt for trashing duplicate copies; refused if any group would lose
   * its last copy. Executed by {@link api.executeDeletion}.
   */
  previewDuplicateRemoval: (scanId: number, paths: readonly string[]) =>
    call<DeletionPlanDto>("preview_duplicate_removal", { scanId, paths }),

  checkFullDiskAccess: () => call<DiskAccess>("check_full_disk_access"),
  openFullDiskAccessSettings: () =>
    call<void>("open_full_disk_access_settings"),
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
  /** Size groups finished, of `groupsTotal`. */
  readonly groupsDone: number;
  readonly groupsTotal: number;
  readonly filesHashed: number;
  readonly bytesRead: number;
}

export const EVENTS = {
  scanProgress: "scan://progress",
  scanDone: "scan://done",
  scanError: "scan://error",
  cleanupProgress: "cleanup://progress",
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
          sink.scanFailed(
            error instanceof Error ? error.message : String(error),
          ),
        );
    }),
    listen<ScanErrorEvent>(EVENTS.scanError, (event) =>
      sink.scanFailed(event.payload.message),
    ),
  ]);
  return () => {
    for (const unlisten of unlisteners) {
      unlisten();
    }
  };
}

/** Follow an executing deletion's progress. Returns a function that stops. */
export function onCleanupProgress(
  handler: (event: CleanupProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<CleanupProgressEvent>(EVENTS.cleanupProgress, (event) =>
    handler(event.payload),
  );
}

/** Follow a duplicate search. Returns a function that stops. */
export function onDupesProgress(
  handler: (event: DupesProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<DupesProgressEvent>(EVENTS.dupesProgress, (event) =>
    handler(event.payload),
  );
}
