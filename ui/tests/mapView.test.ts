// The Map's view model and its URL hash (#26). Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DEFAULT_VIEW,
  cleared,
  fromHash,
  isFiltered,
  toFilter,
  toHash,
  viewKey,
  type MapView,
} from "../src/lib/mapView.ts";
import { useAppStore } from "../src/store.ts";

const busy: MapView = {
  by: "type",
  categories: ["video", "images"],
  minSize: "1g",
  olderThan: "1y",
  within: { id: 42, name: "Down loads & more" },
};

test("the default view is the plain folder map with no filter", () => {
  assert.equal(isFiltered(DEFAULT_VIEW), false);
  assert.deepEqual(toFilter(DEFAULT_VIEW), {});
  assert.equal(toHash("map", DEFAULT_VIEW), "#map");
});

test("filters compose into one backend filter", () => {
  assert.deepEqual(toFilter(busy), {
    categories: ["video", "images"],
    minSize: 1024 ** 3,
    olderThanDays: 365,
    within: 42,
  });
  assert.equal(isFiltered(busy), true);
});

test("a grouping alone is not a filter, and clearing keeps the grouping", () => {
  assert.equal(isFiltered({ ...DEFAULT_VIEW, by: "age" }), false);
  assert.deepEqual(cleared(busy), { ...DEFAULT_VIEW, by: "type" });
});

test("the Folder view ignores a location: its breadcrumb is the location", () => {
  const folder: MapView = { ...DEFAULT_VIEW, within: { id: 3, name: "x" } };
  assert.equal(isFiltered(folder), false);
  assert.deepEqual(toFilter(folder), {});
  assert.equal(toHash("map", folder), "#map");
});

test("a view survives the hash, names with spaces and symbols included", () => {
  const hash = toHash("map", busy);
  const back = fromHash(hash);
  assert.equal(back.route, "map");
  // Categories come back in canonical order; the view is otherwise equal.
  assert.deepEqual(back.view, { ...busy, categories: ["images", "video"] });
  assert.equal(viewKey(back.view), viewKey(busy));
});

test("the key does not depend on the order types were picked in", () => {
  assert.equal(
    viewKey({ ...busy, categories: ["video", "images"] }),
    viewKey({ ...busy, categories: ["images", "video"] }),
  );
  assert.notEqual(viewKey(busy), viewKey({ ...busy, minSize: "10g" }));
});

test("a hand-edited or stale hash falls back instead of failing", () => {
  assert.deepEqual(fromHash(""), { route: null, view: DEFAULT_VIEW });
  assert.deepEqual(fromHash("#large-files"), { route: "large-files", view: DEFAULT_VIEW });
  const odd = fromHash("#map?by=colour&type=video,nope&size=huge&age=1y&in=abc");
  assert.deepEqual(odd.view, { ...DEFAULT_VIEW, categories: ["video"], olderThan: "1y" });
});

const store = () => useAppStore.getState();

test("showing a search result in the Map resets the view to plain folders", () => {
  store().reset();
  store().setMapView(busy);
  store().showInMap(7, false);
  assert.deepEqual(store().mapView, DEFAULT_VIEW);
  assert.equal(store().route, "map");
});

test("switching back to Folder can open a folder at the same time", () => {
  store().reset();
  store().setMapView({ ...DEFAULT_VIEW, categories: ["video"] }, 42);
  assert.deepEqual(store().mapView.categories, ["video"]);
  assert.equal(store().mapFocus?.entryId, 42);
  assert.equal(store().mapFocus?.isDirectory, true);
});

test("a new scan keeps the grouping and filters but drops the old scan's folder", () => {
  store().reset();
  store().setMapView(busy);
  store().scanStarted(9, "/other");
  assert.deepEqual(store().mapView, { ...busy, within: null });
});

test("back/forward restores the screen and the view together", () => {
  store().reset();
  store().restore("map", busy);
  assert.equal(store().route, "map");
  assert.deepEqual(store().mapView, busy);
});
