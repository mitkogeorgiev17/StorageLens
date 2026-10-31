/**
 * Smart-collection actions that call the backend (#28): load the list into the
 * store, save the Map's view, and open a collection against the current scan.
 */

import { api, type CollectionDto } from "./api";
import { queryToView, viewToQuery } from "./collections";
import type { MapView } from "./mapView";
import { useAppStore } from "../store";

/** Reload the saved collections into the store (sidebar, Collections screen). */
export async function refreshCollections(): Promise<readonly CollectionDto[]> {
  const collections = await api.listCollections();
  useAppStore.getState().setCollections(collections);
  return collections;
}

/** Save `view` (limited to `folderPath`, if any) as a new collection. */
export async function saveCollection(
  name: string,
  view: MapView,
  folderPath: string | null,
): Promise<CollectionDto> {
  const saved = await api.createCollection(name, viewToQuery(view, folderPath));
  await refreshCollections();
  return saved;
}

/**
 * Open `collection` on the Map for scan `scanId`. Re-evaluated first, so its
 * folder is resolved in this scan (ids are per scan) — never a stale one.
 * Rejects with a readable reason when it cannot be opened.
 */
export async function openCollection(collection: CollectionDto, scanId: number): Promise<void> {
  if (collection.query === null) {
    throw new Error(collection.problem ?? "This collection cannot be read.");
  }
  const result = await api.evaluateCollection(scanId, collection.id);
  if (result.problem !== null) {
    throw new Error(result.problem);
  }
  const { view, openFolder } = queryToView(collection.query, result.within);
  const store = useAppStore.getState();
  if (store.scan.kind !== "done" || store.scan.summary.scanId !== scanId) {
    // A new scan started while this one was being evaluated.
    return;
  }
  store.openMapView(view, openFolder ?? undefined);
}
