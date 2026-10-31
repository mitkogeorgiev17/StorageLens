/**
 * Smart collections (#28) on the UI side: converting between the Map's view
 * and a saved query, and describing a query in a line. Pure, so it is tested
 * under Node; the calls that save and open live in `collectionActions.ts`.
 *
 * A query stores a folder limit as an absolute path, not an entry id: ids are
 * per scan, paths are not. Opening resolves the path in the current scan.
 */

import type { CollectionQuery, CollectionResultDto } from "./api";
import { formatBytes } from "./format.ts";
import { AGE_OPTIONS, DEFAULT_VIEW, GROUPINGS, type Location, type MapView } from "./mapView.ts";
import { CATEGORY_LABELS } from "./treemap.ts";

/** The query for `view`, limited to the folder at `folderPath` (if any). */
export function viewToQuery(view: MapView, folderPath: string | null): CollectionQuery {
  return {
    v: 1,
    by: view.by,
    chart: view.chart,
    categories: [...view.categories],
    minSize: view.minSize,
    olderThanDays: view.olderThan,
    within: folderPath,
  };
}

/**
 * The Map view a query opens, with its folder resolved in the current scan.
 * In the Folder view a folder is opened rather than filtered by (the
 * breadcrumb is the location), so it comes back as `openFolder`.
 */
export function queryToView(
  query: CollectionQuery,
  folder: Location | null,
): { readonly view: MapView; readonly openFolder: number | null } {
  const view: MapView = {
    ...DEFAULT_VIEW,
    chart: query.chart,
    by: query.by,
    categories: [...query.categories],
    minSize: query.minSize,
    olderThan: query.olderThanDays,
    within: query.by === "folder" ? null : folder,
  };
  return { view, openFolder: query.by === "folder" && folder !== null ? folder.id : null };
}

function lastName(path: string): string {
  const parts = path.split(/[\\/]/).filter((part) => part !== "");
  return parts[parts.length - 1] ?? path;
}

function ageText(days: number): string {
  const preset = Object.values(AGE_OPTIONS).find((option) => option.days === days);
  return preset !== undefined ? preset.label.toLowerCase() : `older than ${days} days`;
}

/** One line: "Video, Images · 500 MB or more · older than 1 year · in Movies · by Type". */
export function describeQuery(query: CollectionQuery): string {
  const parts: string[] = [];
  if (query.categories.length > 0) {
    parts.push(query.categories.map((category) => CATEGORY_LABELS[category]).join(", "));
  } else {
    parts.push("All files");
  }
  if (query.minSize !== null) {
    parts.push(`${formatBytes(query.minSize)} or more`);
  }
  if (query.olderThanDays !== null) {
    parts.push(ageText(query.olderThanDays));
  }
  if (query.within !== null) {
    parts.push(`in ${lastName(query.within)}`);
  }
  const grouping = GROUPINGS.find((entry) => entry.id === query.by)?.label ?? query.by;
  parts.push(`by ${grouping}`);
  return parts.join(" · ");
}

/**
 * The result to show for a collection, or `null`. A result computed against
 * any other scan is never shown, however recent.
 */
export function currentResult(
  results: ReadonlyMap<number, CollectionResultDto>,
  collectionId: number,
  scanId: number | null,
): CollectionResultDto | null {
  const result = results.get(collectionId);
  return result !== undefined && scanId !== null && result.scanId === scanId ? result : null;
}
