// Smart collections on the UI side (#28): view ⇄ query, descriptions, and
// never showing a result from another scan. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { CollectionQuery, CollectionResultDto } from "../src/lib/api.ts";
import { currentResult, describeQuery, queryToView, viewToQuery } from "../src/lib/collections.ts";
import { DEFAULT_VIEW, type MapView } from "../src/lib/mapView.ts";
import { useAppStore } from "../src/store.ts";

const MIB = 1024 * 1024;

const view: MapView = {
  chart: "sunburst",
  by: "type",
  categories: ["video"],
  minSize: 500 * MIB,
  olderThan: 365,
  within: { id: 42, name: "Movies" },
};

test("a view round-trips through a query, with the folder by path", () => {
  const query = viewToQuery(view, "/Users/me/Movies");
  assert.deepEqual(query, {
    v: 1,
    by: "type",
    chart: "sunburst",
    categories: ["video"],
    minSize: 500 * MIB,
    olderThanDays: 365,
    within: "/Users/me/Movies",
  });
  // Opened against a later scan, where the folder has another id.
  const opened = queryToView(query, { id: 7, name: "Movies" });
  assert.deepEqual(opened.view, { ...view, within: { id: 7, name: "Movies" } });
  assert.equal(opened.openFolder, null);
});

test("a Folder-view collection opens its folder instead of filtering by it", () => {
  const query: CollectionQuery = { ...viewToQuery(DEFAULT_VIEW, "/x/Downloads") };
  const opened = queryToView(query, { id: 9, name: "Downloads" });
  assert.equal(opened.view.within, null);
  assert.equal(opened.openFolder, 9);
  assert.equal(queryToView(query, null).openFolder, null);
});

test("descriptions read as one line", () => {
  assert.equal(
    describeQuery(viewToQuery(view, "/Users/me/Movies")),
    "Video · 500 MB or more · older than 1 year · in Movies · by Type",
  );
  assert.equal(describeQuery(viewToQuery({ ...DEFAULT_VIEW, by: "age" }, null)), "All files · by Age");
  assert.match(describeQuery({ ...viewToQuery(DEFAULT_VIEW, null), olderThanDays: 90 }), /older than 90 days/);
  assert.match(describeQuery(viewToQuery(DEFAULT_VIEW, "C:\\Users\\me\\Videos")), /in Videos/);
});

test("a result from another scan is never shown", () => {
  const result: CollectionResultDto = {
    collectionId: 1,
    scanId: 3,
    bytes: 10,
    files: 1,
    within: null,
    problem: null,
  };
  const results = new Map([[1, result]]);
  assert.equal(currentResult(results, 1, 3), result);
  assert.equal(currentResult(results, 1, 4), null, "a newer scan");
  assert.equal(currentResult(results, 1, null), null, "no scan");
  assert.equal(currentResult(results, 2, 3), null);
});

test("opening a collection goes to the Map with its view and folder", () => {
  const store = useAppStore.getState;
  store().reset();
  store().openMapView(view);
  assert.equal(store().route, "map");
  assert.deepEqual(store().mapView, view);
  store().openMapView(DEFAULT_VIEW, 9);
  assert.equal(store().mapFocus?.entryId, 9);
});
