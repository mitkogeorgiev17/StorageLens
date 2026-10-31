// The scan state machine. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { useAppStore, type ScanSummary } from "../src/store.ts";

function summary(scanId: number, status: ScanSummary["status"] = "complete"): ScanSummary {
  return {
    scanId,
    root: "/scan",
    status,
    files: 3,
    dirs: 1,
    bytesAlloc: 12_288,
    elapsedMs: 40,
    skippedDirs: 0,
    elevated: null,
  };
}

const store = () => useAppStore.getState();

test("a scan goes from started to finished", () => {
  store().reset();
  store().scanStarted(7, "/scan");
  assert.equal(store().scan.kind, "scanning");
  store().scanProgressed({
    files: 2,
    dirs: 1,
    bytesAlloc: 8192,
    elapsedMs: 10,
    currentPath: "/scan/a",
  });
  const scanning = store().scan;
  assert.ok(scanning.kind === "scanning" && scanning.progress.files === 2);
  store().scanFinished(summary(7));
  assert.equal(store().scan.kind, "done");
});

test("a done event that beats the start call does not leave the UI scanning", () => {
  store().reset();
  store().scanFinished(summary(8));
  store().scanStarted(8, "/scan");
  const scan = store().scan;
  assert.ok(scan.kind === "done" && scan.summary.scanId === 8);
  // A different, newer scan still starts.
  store().scanStarted(9, "/scan");
  assert.equal(store().scan.kind, "scanning");
});

test("late progress after the end is ignored, and a cancelled scan keeps its totals", () => {
  store().reset();
  store().scanStarted(10, "/scan");
  store().scanFinished(summary(10, "cancelled"));
  store().scanProgressed({ files: 99, dirs: 9, bytesAlloc: 1, elapsedMs: 1, currentPath: null });
  const scan = store().scan;
  assert.ok(scan.kind === "done");
  assert.equal(scan.summary.status, "cancelled");
  assert.equal(scan.summary.files, 3);
});

test("a failure keeps the root it was scanning", () => {
  store().reset();
  store().scanStarted(11, "/data");
  store().scanFailed("disk went away");
  const scan = store().scan;
  assert.ok(scan.kind === "failed" && scan.root === "/data" && scan.message === "disk went away");
});

test("show in map routes to the map and is cleared only by the request it made", () => {
  store().reset();
  store().navigate("overview");
  store().showInMap(42, false);
  assert.equal(store().route, "map");
  const first = store().mapFocus;
  assert.ok(first !== null && first.entryId === 42 && !first.isDirectory);
  store().showInMap(43, true);
  const second = store().mapFocus;
  assert.ok(second !== null && second.nonce > first.nonce);
  store().mapFocusHandled(first.nonce);
  assert.equal(store().mapFocus?.entryId, 43, "a stale acknowledgement keeps the newer request");
  store().mapFocusHandled(second.nonce);
  assert.equal(store().mapFocus, null);
  store().showInMap(44, true);
  store().scanStarted(99, "/scan");
  assert.equal(store().mapFocus, null, "a new scan drops focus requests for the old one");
});
