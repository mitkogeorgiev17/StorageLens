import assert from "node:assert/strict";
import { test } from "node:test";

import type { AppRowDto, AppStatus } from "../src/lib/api.ts";
import { nextSort, removalWarnings, sortApps } from "../src/lib/apps.ts";

function row(
  name: string,
  status: AppStatus,
  lastUsed: number | null,
  totalBytes = 100,
): AppRowDto {
  return {
    id: name,
    name,
    version: null,
    path: `/Applications/${name}.app`,
    appBytes: totalBytes,
    satelliteBytes: 0,
    totalBytes,
    lastUsed,
    useCount: null,
    status,
    running: false,
    autostart: false,
    defaultHandler: false,
    hasUninstaller: false,
    satellites: [],
  };
}

const rows = [
  row("Known", "in_use", 900),
  row("Mystery", "unknown", null),
  row("Old", "unused", 10),
  row("Stale", "likely_unused", 500),
];

test("unknown never sorts in among unused, in either direction", () => {
  const names = (descending: boolean) =>
    sortApps(rows, { key: "status", descending }).map((r) => r.name);
  assert.deepEqual(names(false), ["Old", "Stale", "Known", "Mystery"]);
  assert.deepEqual(names(true), ["Known", "Stale", "Old", "Mystery"]);
});

test("a missing last-used date is not the oldest", () => {
  const names = (descending: boolean) =>
    sortApps(rows, { key: "lastUsed", descending }).map((r) => r.name);
  assert.deepEqual(names(false), ["Old", "Stale", "Known", "Mystery"]);
  assert.deepEqual(names(true), ["Known", "Stale", "Old", "Mystery"]);
});

test("sizes sort biggest first by default, ties by size then name", () => {
  const sized = [
    row("b", "in_use", 1, 5),
    row("a", "in_use", 1, 5),
    row("c", "in_use", 1, 9),
  ];
  const sort = nextSort({ key: "name", descending: false }, "totalBytes");
  assert.deepEqual(sort, { key: "totalBytes", descending: true });
  assert.deepEqual(
    sortApps(sized, sort).map((r) => r.name),
    ["c", "a", "b"],
  );
  assert.deepEqual(nextSort(sort, "totalBytes"), {
    key: "totalBytes",
    descending: false,
  });
});

test("warnings name running, autostart and default-handler apps", () => {
  assert.deepEqual(removalWarnings(row("Quiet", "unused", 1)), []);
  const busy = { ...row("Busy", "in_use", 1), running: true, autostart: true };
  const warnings = removalWarnings(busy);
  assert.equal(warnings.length, 2);
  assert.match(warnings[0] ?? "", /running/);
  const vendor = { ...row("Suite", "in_use", 1), hasUninstaller: true };
  assert.match(removalWarnings(vendor)[0] ?? "", /own uninstaller/);
});
