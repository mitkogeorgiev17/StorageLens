// The Map screen's tree model. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { TreeNodeDto } from "../src/lib/api.ts";
import {
  canZoomInto,
  chainTo,
  indexTree,
  joinPath,
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
