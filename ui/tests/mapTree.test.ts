// The Map screen's tree model. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { TreeNodeDto } from "../src/lib/api.ts";
import {
  canZoomInto,
  chainTo,
  indexTree,
  isSmallerItems,
  joinPath,
  simplifyTree,
  pathOf,
  toTreemapInput,
} from "../src/lib/mapTree.ts";

function node(
  id: number,
  name: string,
  size: number,
  children: TreeNodeDto[] | null = null,
  kind: TreeNodeDto["kind"] = children === null ? "File" : "Dir",
): TreeNodeDto {
  return {
    id,
    name,
    kind,
    category: "other",
    sizeAlloc: size,
    fileCount: 1,
    hasChildren: children !== null && children.length > 0,
    children,
    remainingCount: 0,
    remainingBytes: 0,
  };
}

const tree = node(1, "root", 600, [
  node(2, "photos", 500, [node(3, "2024", 400, [node(4, "a.jpg", 400)]), node(5, "b.png", 100)]),
  node(6, "notes.txt", 100),
]);

test("the index links every node to its parent", () => {
  const index = indexTree(tree);
  assert.equal(index.size, 6);
  assert.equal(index.get(1)?.parent, null);
  assert.equal(index.get(4)?.parent, 3);
  assert.equal(index.get(6)?.parent, 1);
});

test("chains run from below the root to the target", () => {
  const index = indexTree(tree);
  assert.deepEqual(chainTo(index, 3), [
    { id: 2, name: "photos" },
    { id: 3, name: "2024" },
  ]);
  assert.deepEqual(chainTo(index, 1), []);
  assert.deepEqual(chainTo(index, 99), [], "unknown ids have no chain");
});

test("paths join with the root's own separator", () => {
  assert.equal(joinPath("/Users/me", ["a", "b"]), "/Users/me/a/b");
  assert.equal(joinPath("/", ["a"]), "/a");
  assert.equal(joinPath("C:\\", ["Users", "me"]), "C:\\Users\\me");
  assert.equal(joinPath("D:\\data", ["x"]), "D:\\data\\x");
  assert.equal(joinPath("", ["root", "x"]), "root/x");
  const index = indexTree(node(3, "2024", 400, [node(4, "a.jpg", 400)]));
  const crumbs = [
    { id: 1, name: "root" },
    { id: 2, name: "photos" },
    { id: 3, name: "2024" },
  ];
  assert.equal(pathOf("/scan", crumbs, index, 4), "/scan/photos/2024/a.jpg");
  assert.equal(pathOf("/scan", crumbs, index, 3), "/scan/photos/2024");
});

test("treemap input keeps sizes and leaves unexpanded folders as blocks", () => {
  const unexpanded = { ...node(7, "big", 900, null, "Dir"), hasChildren: true };
  const input = toTreemapInput(node(1, "root", 1000, [unexpanded, node(8, "f", 100)]));
  assert.equal(input.size, 1000);
  assert.equal(input.children?.[0]?.children, undefined);
  assert.equal(input.children?.[0]?.size, 900);
  assert.ok(canZoomInto(unexpanded), "an unexpanded folder can still be zoomed into");
  assert.ok(!canZoomInto(node(8, "f", 100)));
});


function total(node: TreeNodeDto): number {
  return (node.children ?? []).reduce((sum, child) => sum + child.sizeAlloc, 0);
}

test("the drawn map folds small items into one tile and keeps every byte", () => {
  const tiny = Array.from({ length: 50 }, (_, i) => node(100 + i, `t${i}`, 10));
  const deep = node(3, "deep", 5_000, [node(31, "d1", 5_000, [node(311, "x", 5_000)])]);
  const big = node(2, "big", 90_000, [node(21, "b1", 60_000), node(22, "b2", 30_000)]);
  const root = { ...node(1, "root", 100_000, [big, deep, ...tiny]), remainingCount: 4, remainingBytes: 4_500 };

  const drawn = simplifyTree(root);
  const kids = drawn.children ?? [];
  assert.deepEqual(
    kids.map((child) => child.name),
    ["big", "deep", "54 smaller items"],
    "50 tiny files plus the backend's 4 remaining fold into one tile",
  );
  const smaller = kids[2];
  assert.ok(smaller !== undefined && isSmallerItems(smaller.id));
  assert.equal(smaller.sizeAlloc, 500 + 4_500);
  assert.equal(total(drawn), 100_000, "no bytes lost");
  assert.equal(drawn.remainingCount, 0);

  const deepDrawn = kids[1];
  assert.ok(deepDrawn !== undefined);
  const d1 = deepDrawn.children?.[0];
  assert.ok(d1 !== undefined);
  assert.equal(d1.children, null, "only two levels drawn");
  assert.ok(canZoomInto(d1), "a folder at the last level still opens");
});

test("a subfolder of only small items is one solid tile", () => {
  const crumbs = Array.from({ length: 20 }, (_, i) => node(10 + i, `c${i}`, 1));
  const root = node(1, "root", 100_000, [node(2, "big", 99_980), node(3, "crumbs", 20, crumbs)]);
  const crumbsDrawn = simplifyTree(root).children?.find((child) => child.name === "crumbs");
  assert.ok(crumbsDrawn === undefined || crumbsDrawn.children === null);
});

test("smaller-items ids never collide with group ids", () => {
  assert.equal(isSmallerItems(-1), false);
  assert.equal(isSmallerItems(-1_000_000), false);
  assert.equal(isSmallerItems(7), false);
});
