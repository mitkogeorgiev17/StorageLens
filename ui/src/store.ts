/**
 * App state (AGENTS.md §8: zustand, no context spaghetti).
 *
 * What lives here:
 *
 * - **The current route.** A desktop window has no address bar, so navigation is
 *   plain state rather than a router dependency. `lib/history.ts` mirrors it,
 *   and the Map's view, into the URL hash for back/forward.
 * - **The Map's view** (#26): grouping and filters, kept across screens.
 * - **The scan session**, as an explicit state machine. The Tauri command
 *   surface (#18) drives it from backend events; screens only read it.
 * - **The refresh after a cleanup** (#97): a rescan of the same root that runs
 *   while the finished scan stays on screen, and the last move to the Trash,
 *   so its undo survives that rescan.
 *
 * Nothing here holds rows: tables page through the catalog (AGENTS.md §8, ~200
 * rows per component).
 */

import { create } from "zustand";

import type { DeletionResultDto, UndoResultDto } from "./lib/api";
import { DEFAULT_VIEW, type MapView } from "./lib/mapView.ts";

/** Every screen in the sidebar, in display order. */
export const ROUTE_IDS = [
  "overview",
  "map",
  "large-files",
  "apps",
  "startup",
  "cleanup",
  "history",
] as const;

export type RouteId = (typeof ROUTE_IDS)[number];

/** Live counters while a scan runs; mirrors core's `Progress`. */
export interface ScanProgress {
  readonly files: number;
  readonly dirs: number;
  /** Allocated bytes seen so far — the "space used" number. */
  readonly bytesAlloc: number;
  readonly elapsedMs: number;
  readonly currentPath: string | null;
}

/** What a finished scan reports; mirrors core's `ScanResult`. */
export interface ScanSummary {
  readonly scanId: number;
  readonly root: string;
  readonly status: "complete" | "partial" | "cancelled";
  readonly files: number;
  readonly dirs: number;
  readonly bytesAlloc: number;
  readonly elapsedMs: number;
  readonly skippedDirs: number;
  /** `false` on an unelevated Windows scan; `null` where it does not apply. */
  readonly elevated: boolean | null;
}

export type ScanState =
  | { readonly kind: "idle" }
  | {
      readonly kind: "scanning";
      readonly scanId: number;
      readonly root: string;
      readonly progress: ScanProgress;
    }
  | { readonly kind: "done"; readonly summary: ScanSummary }
  | { readonly kind: "failed"; readonly root: string; readonly message: string };

const EMPTY_PROGRESS: ScanProgress = {
  files: 0,
  dirs: 0,
  bytesAlloc: 0,
  elapsedMs: 0,
  currentPath: null,
};

/** A background rescan after the Trash changed (#97). */
export interface Refresh {
  /** `null` until `start_scan` returns. */
  readonly scanId: number | null;
  readonly progress: ScanProgress;
  /** The Trash changed again while it ran: rescan once more when it ends. */
  readonly queued: boolean;
}

/** The last move to the Trash, and its undo once asked for. */
export interface LastTrash {
  readonly result: DeletionResultDto;
  readonly undone: UndoResultDto | null;
}

/** A request to open the Map on an entry (from search). */
export interface MapFocus {
  readonly entryId: number;
  readonly isDirectory: boolean;
  /** Distinguishes two requests for the same entry. */
  readonly nonce: number;
}

export interface AppState {
  readonly route: RouteId;
  readonly scan: ScanState;
  readonly mapFocus: MapFocus | null;
  readonly mapView: MapView;
  readonly refresh: Refresh | null;
  /** Why the last refresh did not replace the data; cleared by the next scan. */
  readonly staleReason: string | null;
  readonly lastTrash: LastTrash | null;
  navigate(route: RouteId): void;
  /**
   * Go to the Map, showing `entryId` (a folder opened, a file selected) in the
   * plain folder view: a filtered or grouped view might not contain it. The
   * chart (treemap or sunburst) stays.
   */
  showInMap(entryId: number, isDirectory: boolean): void;
  /** Change the Map's view; with `openFolder`, open that folder in it. */
  setMapView(view: MapView, openFolder?: number): void;
  /** Back/forward: the screen and view a history entry recorded. */
  restore(route: RouteId, view: MapView): void;
  /** The Map applied a focus request; clear it if it is still the current one. */
  mapFocusHandled(nonce: number): void;
  scanStarted(scanId: number, root: string): void;
  scanProgressed(progress: ScanProgress): void;
  scanFinished(summary: ScanSummary): void;
  scanFailed(message: string): void;
  /**
   * The Trash changed. Returns `true` when the caller should start a rescan
   * now (then report it with `refreshStarted`); `false` when one is running
   * (it is queued) or there is no finished scan to refresh.
   */
  refreshRequested(): boolean;
  refreshStarted(scanId: number): void;
  /** `start_scan` refused or failed: keep the data, say why. */
  refreshAborted(reason: string): void;
  trashed(result: DeletionResultDto): void;
  untrashed(undone: UndoResultDto): void;
  trashDismissed(): void;
  reset(): void;
}

export const useAppStore = create<AppState>()((set) => ({
  route: "overview",
  scan: { kind: "idle" },
  mapFocus: null,
  mapView: DEFAULT_VIEW,
  refresh: null,
  staleReason: null,
  lastTrash: null,

  navigate: (route) => set({ route }),

  showInMap: (entryId, isDirectory) =>
    set((state) => ({
      route: "map",
      mapView: { ...DEFAULT_VIEW, chart: state.mapView.chart },
      mapFocus: { entryId, isDirectory, nonce: (state.mapFocus?.nonce ?? 0) + 1 },
    })),

  setMapView: (view, openFolder) =>
    set((state) =>
      openFolder === undefined
        ? { mapView: view }
        : {
            mapView: view,
            mapFocus: {
              entryId: openFolder,
              isDirectory: true,
              nonce: (state.mapFocus?.nonce ?? 0) + 1,
            },
          },
    ),

  restore: (route, view) => set({ route, mapView: view }),


  mapFocusHandled: (nonce) =>
    set((state) => (state.mapFocus?.nonce === nonce ? { mapFocus: null } : state)),

  // A tiny scan can finish — and its `scan://done` be handled — before the
  // `start_scan` call that started it returns. Starting after finishing would
  // leave the UI stuck on "scanning", so a finished scan stays finished.
  scanStarted: (scanId, root) =>
    set((state) =>
      state.scan.kind === "done" && state.scan.summary.scanId >= scanId
        ? state
        : {
            scan: { kind: "scanning", scanId, root, progress: EMPTY_PROGRESS },
            refresh: null,
            staleReason: null,
            mapFocus: null,
            // A folder of the old scan means nothing in the new one.
            mapView: { ...state.mapView, within: null },
          },
    ),

  // Progress for a scan that is no longer running (a late event after
  // completion or failure) is ignored rather than resurrecting it.
  scanProgressed: (progress) =>
    set((state) =>
      state.scan.kind === "scanning"
        ? { scan: { ...state.scan, progress } }
        : state.refresh !== null
          ? { refresh: { ...state.refresh, progress } }
          : state,
    ),

  // Only the scan in flight (or a newer one) replaces what is on screen. A
  // refresh that was stopped keeps the old, complete data.
  scanFinished: (summary) =>
    set((state) => {
      const current =
        state.scan.kind === "scanning"
          ? state.scan.scanId
          : state.scan.kind === "done"
            ? state.scan.summary.scanId
            : null;
      if (state.scan.kind === "scanning" && summary.scanId !== current) {
        return state;
      }
      if (state.scan.kind === "done" && current !== null && summary.scanId <= current) {
        return state;
      }
      if (state.refresh !== null && state.scan.kind === "done" && summary.status === "cancelled") {
        return {
          refresh: null,
          staleReason: "The refresh was stopped, so this is the scan from before the cleanup.",
        };
      }
      const keepView = state.refresh !== null && state.scan.kind === "done";
      return {
        scan: { kind: "done", summary },
        refresh: null,
        staleReason: null,
        // A refresh scans the same root: the open folder means nothing in the
        // new scan, but the grouping and filters still apply.
        ...(keepView ? { mapFocus: null, mapView: { ...state.mapView, within: null } } : {}),
      };
    }),

  scanFailed: (message) =>
    set((state) =>
      state.refresh !== null && state.scan.kind === "done"
        ? { refresh: null, staleReason: `Could not refresh after the cleanup: ${message}` }
        : {
            scan: {
              kind: "failed",
              root: state.scan.kind === "scanning" ? state.scan.root : "",
              message,
            },
          },
    ),

  refreshRequested: () => {
    let start = false;
    set((state) => {
      if (state.refresh !== null) {
        return { refresh: { ...state.refresh, queued: true } };
      }
      if (state.scan.kind !== "done") {
        return state;
      }
      start = true;
      return { refresh: { scanId: null, progress: EMPTY_PROGRESS, queued: false } };
    });
    return start;
  },

  refreshStarted: (scanId) =>
    set((state) =>
      // A tiny rescan can finish before `start_scan` returns.
      state.refresh === null || (state.scan.kind === "done" && state.scan.summary.scanId >= scanId)
        ? state
        : { refresh: { ...state.refresh, scanId } },
    ),

  refreshAborted: (reason) => set({ refresh: null, staleReason: reason }),

  trashed: (result) => set({ lastTrash: { result, undone: null } }),

  untrashed: (undone) =>
    set((state) => (state.lastTrash === null ? state : { lastTrash: { ...state.lastTrash, undone } })),

  trashDismissed: () => set({ lastTrash: null }),

  reset: () =>
    set({
      scan: { kind: "idle" },
      mapFocus: null,
      mapView: DEFAULT_VIEW,
      refresh: null,
      staleReason: null,
      lastTrash: null,
    }),
}));

/** Whether a finished scan exists for screens to show. */
export function hasScan(scan: ScanState): scan is Extract<ScanState, { kind: "done" }> {
  return scan.kind === "done";
}
