/**
 * App state (AGENTS.md §8: zustand, no context spaghetti).
 *
 * Four things live here today:
 *
 * - **The current route.** A desktop window has no address bar, so navigation is
 *   plain state rather than a router dependency. `lib/history.ts` mirrors it,
 *   and the Map's view, into the URL hash for back/forward.
 * - **The Map's view** (#26): grouping and filters, kept across screens.
 * - **Saved smart collections** (#28), for the sidebar and the Collections
 *   screen. Their results are not here: they belong to one scan each.
 * - **The scan session**, as an explicit state machine. The Tauri command
 *   surface (#18) drives it from backend events; screens only read it.
 *
 * Nothing here holds rows: tables page through the catalog (AGENTS.md §8, ~200
 * rows per component).
 */

import { create } from "zustand";

import type { CollectionDto } from "./lib/api";
import { DEFAULT_VIEW, type MapView } from "./lib/mapView.ts";

/** Every screen in the sidebar, in display order. */
export const ROUTE_IDS = [
  "overview",
  "map",
  "large-files",
  "collections",
  "apps",
  "cleanup",
  "duplicates",
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
  /** Saved smart collections (#28); `null` until first loaded. */
  readonly collections: readonly CollectionDto[] | null;
  navigate(route: RouteId): void;
  /**
   * Go to the Map, showing `entryId` (a folder opened, a file selected) in the
   * plain folder view: a filtered or grouped view might not contain it. The
   * chart (treemap or sunburst) stays.
   */
  showInMap(entryId: number, isDirectory: boolean): void;
  /** Change the Map's view; with `openFolder`, open that folder in it. */
  setMapView(view: MapView, openFolder?: number): void;
  /** Go to the Map showing `view` (a collection opened); see `setMapView`. */
  openMapView(view: MapView, openFolder?: number): void;
  /** Back/forward: the screen and view a history entry recorded. */
  restore(route: RouteId, view: MapView): void;
  setCollections(collections: readonly CollectionDto[]): void;
  /** The Map applied a focus request; clear it if it is still the current one. */
  mapFocusHandled(nonce: number): void;
  scanStarted(scanId: number, root: string): void;
  scanProgressed(progress: ScanProgress): void;
  scanFinished(summary: ScanSummary): void;
  scanFailed(message: string): void;
  reset(): void;
}

export const useAppStore = create<AppState>()((set) => ({
  route: "overview",
  scan: { kind: "idle" },
  mapFocus: null,
  mapView: DEFAULT_VIEW,
  collections: null,

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

  openMapView: (view, openFolder) =>
    set((state) => ({
      route: "map",
      mapView: view,
      mapFocus:
        openFolder === undefined
          ? state.mapFocus
          : { entryId: openFolder, isDirectory: true, nonce: (state.mapFocus?.nonce ?? 0) + 1 },
    })),

  restore: (route, view) => set({ route, mapView: view }),

  setCollections: (collections) => set({ collections }),

  mapFocusHandled: (nonce) =>
    set((state) => (state.mapFocus?.nonce === nonce ? { mapFocus: null } : state)),

  // A tiny scan can finish — and its `scan://done` be handled — before the
  // `start_scan` call that started it returns. Starting after finishing would
  // leave the UI stuck on "scanning", so a finished scan stays finished.
  scanStarted: (scanId, root) =>
    set((state) =>
      state.scan.kind === "done" && state.scan.summary.scanId === scanId
        ? state
        : {
            scan: { kind: "scanning", scanId, root, progress: EMPTY_PROGRESS },
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
        : state,
    ),

  scanFinished: (summary) => set({ scan: { kind: "done", summary } }),

  scanFailed: (message) =>
    set((state) => ({
      scan: {
        kind: "failed",
        root: state.scan.kind === "scanning" ? state.scan.root : "",
        message,
      },
    })),

  reset: () => set({ scan: { kind: "idle" }, mapFocus: null, mapView: DEFAULT_VIEW }),
}));

/** Whether a finished scan exists for screens to show. */
export function hasScan(scan: ScanState): scan is Extract<ScanState, { kind: "done" }> {
  return scan.kind === "done";
}
