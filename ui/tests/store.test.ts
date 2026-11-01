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

const PROGRESS = { files: 5, dirs: 1, bytesAlloc: 4096, elapsedMs: 5, currentPath: null };

test("a refresh keeps the finished scan on screen until the new one is done", () => {
  store().reset();
  store().scanFinished(summary(1));
  assert.equal(store().refreshRequested(), true);
  store().refreshStarted(2);
  store().scanProgressed(PROGRESS);
  const during = store().scan;
  assert.ok(during.kind === "done" && during.summary.scanId === 1, "old data stays");
  assert.equal(store().refresh?.progress.files, 5);
  store().scanFinished(summary(2));
  const after = store().scan;
  assert.ok(after.kind === "done" && after.summary.scanId === 2);
  assert.equal(store().refresh, null);
  assert.equal(store().staleReason, null);
});

test("a second change while refreshing queues one more instead of starting", () => {
  store().reset();
  store().scanFinished(summary(1));
  assert.equal(store().refreshRequested(), true);
  store().refreshStarted(2);
  assert.equal(store().refreshRequested(), false);
  assert.equal(store().refreshRequested(), false);
  assert.equal(store().refresh?.queued, true);
});

test("a stopped or failed refresh keeps the old data and says why", () => {
  store().reset();
  store().scanFinished(summary(1));
  store().refreshRequested();
  store().refreshStarted(2);
  store().scanFinished(summary(2, "cancelled"));
  const stopped = store().scan;
  assert.ok(stopped.kind === "done" && stopped.summary.scanId === 1);
  assert.match(store().staleReason ?? "", /stopped/);

  store().refreshRequested();
  store().refreshStarted(3);
  store().scanFailed("disk gone");
  assert.equal(store().scan.kind, "done");
  assert.match(store().staleReason ?? "", /disk gone/);
});

test("a refresh that finishes before start_scan returns is not left running", () => {
  store().reset();
  store().scanFinished(summary(1));
  store().refreshRequested();
  store().scanFinished(summary(2));
  store().refreshStarted(2);
  assert.equal(store().refresh, null);
  const scan = store().scan;
  assert.ok(scan.kind === "done" && scan.summary.scanId === 2);
});

test("an older scan's late result never replaces a newer one", () => {
  store().reset();
  store().scanFinished(summary(5));
  store().scanFinished(summary(4));
  const scan = store().scan;
  assert.ok(scan.kind === "done" && scan.summary.scanId === 5);
});

test("nothing to refresh without a finished scan", () => {
  store().reset();
  assert.equal(store().refreshRequested(), false);
  assert.equal(store().refresh, null);
});
