import assert from "node:assert/strict";
import { test } from "node:test";

import type { DupFileDto, DupGroupDto } from "../src/lib/api.ts";
import {
  canSelect,
  keepOnly,
  prune,
  selectionTotal,
  suggestKeep,
  toggle,
  type DupSelection,
} from "../src/lib/duplicates.ts";

const file = (path: string, modified: number | null = 0): DupFileDto => ({
  path,
  sizeAlloc: 10,
  modified,
});

const group = (hash: string, paths: string[]): DupGroupDto => ({
  hash,
  sizeLogical: 10,
  reclaimable: 10 * (paths.length - 1),
  files: paths.map((path) => file(path)),
});

test("the suggestion prefers Documents, then newest, then shallowest", () => {
  assert.equal(
    suggestKeep([
      file("/Users/u/Downloads/a.mov", 900),
      file("/Users/u/Documents/deep/er/a.mov", 100),
      file("/Users/u/.Trash/a.mov", 999),
    ]),
    "/Users/u/Documents/deep/er/a.mov",
  );
  assert.equal(
    suggestKeep([file("/a/b/c/x", 5), file("/a/x", 5), file("/a/b/x", 9)]),
    "/a/b/x",
    "newest wins over shallowest",
  );
  assert.equal(suggestKeep([file("/a/b/x", 5), file("/a/x", 5)]), "/a/x");
  assert.equal(
    suggestKeep([
      file("C:\\Users\\u\\Downloads\\x", 9),
      file("C:\\Users\\u\\Videos\\x", 1),
    ]),
    "C:\\Users\\u\\Videos\\x",
  );
  assert.equal(
    suggestKeep([file("/$Recycle.Bin/x", 9), file("/x", null)]),
    "/x",
  );
});

test("every copy of a group can never be selected", () => {
  const g = group("h", ["/a", "/b", "/c"]);
  let selection: DupSelection = new Map();
  selection = toggle(selection, g, "/a");
  selection = toggle(selection, g, "/b");
  assert.equal(canSelect(selection, g, "/c"), false);
  assert.equal(
    toggle(selection, g, "/c"),
    selection,
    "the last copy is refused",
  );
  assert.equal(
    canSelect(selection, g, "/a"),
    true,
    "unticking is always allowed",
  );
  selection = toggle(selection, g, "/a");
  assert.deepEqual([...(selection.get("h") ?? [])], ["/b"]);
});

test("keepOnly ticks the rest and totals add up", () => {
  const a = group("a", ["/1", "/2", "/3"]);
  const b = group("b", ["/4", "/5"]);
  let selection = keepOnly(new Map(), a, "/2");
  selection = toggle(selection, b, "/5");
  assert.deepEqual(selectionTotal(selection, [a, b]), {
    paths: ["/1", "/3", "/5"],
    bytes: 30,
  });
});

test("pruning drops vanished copies and never leaves a group fully ticked", () => {
  const before = group("a", ["/1", "/2", "/3"]);
  const selection = keepOnly(new Map(), before, "/3");
  // /3 went to the Trash elsewhere: /1 and /2 are all that is left.
  const after = group("a", ["/1", "/2"]);
  assert.equal(prune(selection, [after]).size, 0);
  assert.equal(prune(selection, []).size, 0);
  const partial = prune(toggle(new Map(), before, "/1"), [after]);
  assert.deepEqual([...(partial.get("a") ?? [])], ["/1"]);
});
