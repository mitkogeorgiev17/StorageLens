/**
 * Keep the data fresh (#97, #171).
 *
 * The app rescans the scanned root in the background on launch, and after
 * every action that changes the disk: a move to the Trash, an undo, a Docker
 * removal, system leftovers, an uninstall. The finished scan stays on screen until the new one replaces it
 * (`store.scanFinished`), so no screen goes blank. One scan runs at a time: a
 * second change while a refresh runs queues exactly one more.
 */

import type { DeletionResultDto, UndoResultDto } from "./api";
import { api } from "./api";
import { useAppStore } from "../store";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Rescan the current root, or queue a rescan behind the one running. */
export async function requestRescan(): Promise<void> {
  const store = useAppStore.getState();
  if (!store.refreshRequested() || store.scan.kind !== "done") {
    return;
  }
  try {
    store.refreshStarted(await api.startScan(store.scan.summary.root));
  } catch (error) {
    useAppStore.getState().refreshAborted(`Could not refresh: ${message(error)}`);
  }
}

/** Something moved to the Trash: remember it for undo, then rescan. */
export function afterTrash(result: DeletionResultDto): void {
  useAppStore.getState().trashed(result);
  if (result.trashed > 0) {
    void requestRescan();
  }
}

/** An undo ran: record it, then rescan. */
export function afterUndo(undone: UndoResultDto): void {
  useAppStore.getState().untrashed(undone);
  void requestRescan();
}

/** Start the queued rescan when the running one ends. Call once at startup. */
export function installRescanQueue(): () => void {
  return useAppStore.subscribe((state, previous) => {
    if (previous.refresh?.queued === true && state.refresh === null) {
      void requestRescan();
    }
  });
}

/** Stop a running refresh; the scan from before stays. */
export function stopRefresh(): void {
  const scanId = useAppStore.getState().refresh?.scanId;
  if (scanId !== null && scanId !== undefined) {
    api.cancelScan(scanId).catch(() => undefined);
  }
}
