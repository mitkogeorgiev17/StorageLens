/**
 * App state (AGENTS.md §8: zustand, no context spaghetti).
 *
 * Two things live here today:
 *
 * - **The current route.** A desktop window has no address bar, so navigation is
 *   plain state rather than a router dependency.
 * - **The scan session**, as an explicit state machine. The Tauri command
 *   surface (#18) drives it from backend events; screens only read it.
 *
 * Nothing here holds rows: tables page through the catalog (AGENTS.md §8, ~200
 * rows per component).
 */

import { create } from "zustand";

/** Every screen in the sidebar, in display order. */
export const ROUTE_IDS = [
  "overview",
  "map",
  "large-files",
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
  | { readonly kind: "scanning"; readonly root: string; readonly progress: ScanProgress }
  | { readonly kind: "done"; readonly summary: ScanSummary }
  | { readonly kind: "failed"; readonly root: string; readonly message: string };

const EMPTY_PROGRESS: ScanProgress = {
  files: 0,
  dirs: 0,
  bytesAlloc: 0,
  elapsedMs: 0,
  currentPath: null,
};

export interface AppState {
  readonly route: RouteId;
  readonly scan: ScanState;
  navigate(route: RouteId): void;
  scanStarted(root: string): void;
  scanProgressed(progress: ScanProgress): void;
  scanFinished(summary: ScanSummary): void;
  scanFailed(message: string): void;
  reset(): void;
}

export const useAppStore = create<AppState>()((set) => ({
  route: "overview",
  scan: { kind: "idle" },

  navigate: (route) => set({ route }),

  scanStarted: (root) =>
    set({ scan: { kind: "scanning", root, progress: EMPTY_PROGRESS } }),

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

  reset: () => set({ scan: { kind: "idle" } }),
}));

/** Whether a finished scan exists for screens to show. */
export function hasScan(scan: ScanState): scan is Extract<ScanState, { kind: "done" }> {
  return scan.kind === "done";
}
