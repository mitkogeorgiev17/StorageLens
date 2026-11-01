import assert from "node:assert/strict";
import { test } from "node:test";

import type { RuleGroupDto } from "../src/lib/api.ts";
import {
  CONFIRM_ABOVE,
  countBy,
  groupState,
  initialSelection,
  refusedForPermission,
  selectionTotal,
  sizeIndex,
  toggleGroup,
  toggleItem,
  topLevel,
} from "../src/lib/cleanup.ts";

function group(
  id: string,
  deletable: boolean,
  items: [string, number, boolean][],
): RuleGroupDto {
  return {
    ruleId: id,
    title: id,
    kind: deletable ? "regenerable" : "system",
    risk: deletable ? "safe" : "never",
    explain: "",
    command: null,
    deletable,
    bytes: items.reduce((sum, [, bytes]) => sum + bytes, 0),
    items: items.map(([path, bytes, selected]) => ({
      path,
      entryId: null,
      bytes,
      selected,
    })),
  };
}

const caches = group("caches", true, [
  ["/u/Library/Caches/a", 100, true],
  ["/u/Library/Caches/b", 50, false],
]);
const homebrew = group("homebrew", true, [
  ["/u/Library/Caches/a/brew", 30, true],
]);
const swap = group("swap", false, [["/private/var/vm", 4096, true]]);

test("only deletable, preselected items start selected", () => {
  const selected = initialSelection([caches, homebrew, swap]);
  assert.deepEqual([...selected].sort(), [
    "/u/Library/Caches/a",
    "/u/Library/Caches/a/brew",
  ]);
});

test("cards toggle whole, items one at a time", () => {
  let selected = initialSelection([caches]);
  assert.equal(groupState(selected, caches), "some");
  selected = toggleGroup(selected, caches);
  assert.equal(groupState(selected, caches), "all");
  selected = toggleGroup(selected, caches);
  assert.equal(groupState(selected, caches), "none");
  selected = toggleItem(selected, "/u/Library/Caches/b");
  assert.equal(groupState(selected, caches), "some");
  assert.equal(
    toggleGroup(selected, swap),
    selected,
    "report-only cards cannot be selected",
  );
});

test("a selected folder covers what is selected inside it", () => {
  const selected = initialSelection([caches, homebrew]);
  assert.deepEqual(topLevel(selected), ["/u/Library/Caches/a"]);
  assert.deepEqual(selectionTotal(selected, sizeIndex([caches, homebrew])), {
    items: 1,
    bytes: 100,
  });
  const windows = new Set([
    "C:\\Users\\me\\Temp",
    "C:\\Users\\me\\Temp\\x",
    "C:\\Users\\me\\Temp2",
  ]);
  assert.deepEqual(topLevel(windows), [
    "C:\\Users\\me\\Temp",
    "C:\\Users\\me\\Temp2",
  ]);
});

test("the running total updates well within 50 ms for 20,000 items", () => {
  const items: [string, number, boolean][] = [];
  for (let i = 0; i < 20_000; i += 1) {
    items.push([`/u/projects/p${i}/node_modules`, 1_000 + i, true]);
  }
  const big = group("node", true, items);
  const sizes = sizeIndex([big]);
  let selected = initialSelection([big]);
  const started = performance.now();
  selected = toggleItem(selected, "/u/projects/p7/node_modules");
  const total = selectionTotal(selected, sizes);
  const elapsed = performance.now() - started;
  assert.equal(total.items, 19_999);
  assert.ok(elapsed < 50, `${elapsed.toFixed(1)} ms`);
});

test("plans over 5 GB need a second confirmation", () => {
  assert.equal(CONFIRM_ABOVE, 5 * 1024 ** 3);
});

test("results are counted by status in a fixed order", () => {
  const items = [
    { status: "skipped" },
    { status: "trashed" },
    { status: "skipped" },
  ];
  assert.deepEqual(
    countBy(items, (item) => item.status, ["trashed", "refused", "skipped"]),
    [
      { value: "trashed", count: 1 },
      { value: "skipped", count: 2 },
    ],
  );
});

test("a permission refusal asks for Full Disk Access; other failures do not", () => {
  assert.equal(
    refusedForPermission([
      null,
      "Error during a `trash` operation: … you don’t have the necessary permission. (-5000)",
    ]),
    true,
  );
  assert.equal(refusedForPermission(["Operation not permitted (os error 1)"]), true);
  assert.equal(refusedForPermission(["changed since the scan", null]), false);
});
