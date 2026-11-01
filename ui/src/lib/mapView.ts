/**
 * What the Map shows (#26): a grouping plus composable filters, independent of
 * React so it is tested under Node.
 *
 * The view lives in the store and in the window's URL hash
 * (`#map?by=type&type=video,images&size=1g&age=1y&in=42:Downloads`), so it
 * survives switching screens and back/forward. The hash is parsed leniently:
 * an unknown value falls back to "no filter" rather than an error.
 */

import type { Category, Filter, Grouping } from "./api";

/** A folder the view is limited to: its entry id, and its name for the chip. */
export interface Location {
  readonly id: number;
  readonly name: string;
}

/** How the Map draws the tree (#27). Not part of what is loaded. */
export type Chart = "treemap" | "sunburst";

export const CHARTS: readonly { readonly id: Chart; readonly label: string }[] = [
  { id: "treemap", label: "Treemap" },
  { id: "sunburst", label: "Sunburst" },
];

export interface MapView {
  readonly chart: Chart;
  readonly by: Grouping;
  /** Empty means every type. */
  readonly categories: readonly Category[];
  /** Allocated bytes, at least. Usually a {@link SIZE_OPTIONS} preset; a
   * view restored from history may carry any value. */
  readonly minSize: number | null;
  /** Days since last modified, at least. Usually an {@link AGE_OPTIONS} preset. */
  readonly olderThan: number | null;
  /** Only files in this folder. Applies to every grouping but Folder, where
   * the breadcrumb is the location. */
  readonly within: Location | null;
}

export const DEFAULT_VIEW: MapView = {
  chart: "treemap",
  by: "folder",
  categories: [],
  minSize: null,
  olderThan: null,
  within: null,
};

export const GROUPINGS: readonly { readonly id: Grouping; readonly label: string }[] = [
  { id: "folder", label: "Folder" },
  { id: "type", label: "Type" },
  { id: "app", label: "App" },
  { id: "age", label: "Age" },
  { id: "size", label: "Size" },
  { id: "cleanliness", label: "Cleanliness" },
];

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

/** Presets, by the short name the URL hash uses for them. */
export const SIZE_OPTIONS = {
  "10m": { label: "10 MB or more", bytes: 10 * MIB },
  "100m": { label: "100 MB or more", bytes: 100 * MIB },
  "500m": { label: "500 MB or more", bytes: 500 * MIB },
  "1g": { label: "1 GB or more", bytes: GIB },
  "10g": { label: "10 GB or more", bytes: 10 * GIB },
} as const;

export const AGE_OPTIONS = {
  "1m": { label: "Older than 1 month", days: 30 },
  "6m": { label: "Older than 6 months", days: 182 },
  "1y": { label: "Older than 1 year", days: 365 },
  "2y": { label: "Older than 2 years", days: 730 },
} as const;

const CATEGORIES: readonly Category[] = [
  "images",
  "video",
  "audio",
  "documents",
  "spreadsheets",
  "presentations",
  "archives",
  "code",
  "data",
  "fonts",
  "three-d",
  "apps",
  "disk-images",
  "caches",
  "app-data",
  "system",
  "developer",
  "other",
];
export const ALL_CATEGORIES = CATEGORIES;

/** Whether any filter narrows the view (the grouping alone does not). */
export function isFiltered(view: MapView): boolean {
  return (
    view.categories.length > 0 ||
    view.minSize !== null ||
    view.olderThan !== null ||
    (view.within !== null && view.by !== "folder")
  );
}

/** The backend filter for `view`. */
export function toFilter(view: MapView): Filter {
  return {
    ...(view.categories.length > 0 ? { categories: view.categories } : {}),
    ...(view.minSize !== null ? { minSize: view.minSize } : {}),
    ...(view.olderThan !== null ? { olderThanDays: view.olderThan } : {}),
    ...(view.within !== null && view.by !== "folder" ? { within: view.within.id } : {}),
  };
}

/** The view without any filter, keeping the grouping and the chart. */
export function cleared(view: MapView): MapView {
  return { ...DEFAULT_VIEW, chart: view.chart, by: view.by };
}

/**
 * A stable key for what the view loads: equal data, equal keys (categories in
 * canonical order). The chart is left out — both draw the same tree.
 */
export function viewKey(view: MapView): string {
  return toQuery({ ...view, chart: "treemap" }).toString();
}

function toQuery(view: MapView): URLSearchParams {
  const query = new URLSearchParams();
  if (view.chart !== "treemap") {
    query.set("chart", view.chart);
  }
  if (view.by !== "folder") {
    query.set("by", view.by);
  }
  if (view.categories.length > 0) {
    const chosen = new Set(view.categories);
    query.set("type", CATEGORIES.filter((category) => chosen.has(category)).join(","));
  }
  if (view.minSize !== null) {
    query.set("size", sizeName(view.minSize));
  }
  if (view.olderThan !== null) {
    query.set("age", ageName(view.olderThan));
  }
  if (view.within !== null && view.by !== "folder") {
    query.set("in", `${view.within.id}:${view.within.name}`);
  }
  return query;
}

/** A preset's short name, or the plain number. */
export function sizeName(bytes: number): string {
  return Object.entries(SIZE_OPTIONS).find(([, option]) => option.bytes === bytes)?.[0] ?? String(bytes);
}

export function ageName(days: number): string {
  return Object.entries(AGE_OPTIONS).find(([, option]) => option.days === days)?.[0] ?? String(days);
}

/** A positive whole number from the hash, or `null`. */
function count(text: string | null): number | null {
  if (text === null || !/^\d{1,15}$/.test(text)) {
    return null;
  }
  const value = Number(text);
  return value > 0 ? value : null;
}

function isKey<T extends object>(table: T, key: string | null): key is Extract<keyof T, string> {
  return key !== null && Object.hasOwn(table, key);
}

function parseView(query: URLSearchParams): MapView {
  const by = query.get("by");
  const grouping = GROUPINGS.find((entry) => entry.id === by)?.id ?? "folder";
  const types = new Set((query.get("type") ?? "").split(","));
  const size = query.get("size");
  const age = query.get("age");
  const location = /^(\d+):(.*)$/s.exec(query.get("in") ?? "");
  const chart = CHARTS.find((entry) => entry.id === query.get("chart"))?.id ?? "treemap";
  return {
    chart,
    by: grouping,
    categories: CATEGORIES.filter((category) => types.has(category)),
    minSize: isKey(SIZE_OPTIONS, size) ? SIZE_OPTIONS[size].bytes : count(size),
    olderThan: isKey(AGE_OPTIONS, age) ? AGE_OPTIONS[age].days : count(age),
    within:
      location === null || grouping === "folder"
        ? null
        : { id: Number(location[1]), name: location[2] ?? "" },
  };
}

/** The URL hash for a screen and the Map's view, e.g. `#map?by=type`. */
export function toHash(route: string, view: MapView): string {
  const query = toQuery(view).toString();
  return query === "" ? `#${route}` : `#${route}?${query}`;
}

/** The screen and view a hash names; `null` route when it names none. */
export function fromHash(hash: string): { readonly route: string | null; readonly view: MapView } {
  const body = hash.startsWith("#") ? hash.slice(1) : hash;
  const mark = body.indexOf("?");
  const route = mark === -1 ? body : body.slice(0, mark);
  const query = new URLSearchParams(mark === -1 ? "" : body.slice(mark + 1));
  return { route: route === "" ? null : route, view: parseView(query) };
}
