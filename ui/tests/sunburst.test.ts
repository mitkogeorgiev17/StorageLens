// Sunburst layout, level of detail, hit-testing, and keyboard movement (#27),
// plus the 1M-node frame benchmark. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  FULL_VIEW,
  MAX_ARCS,
  ancestorPath,
  arcOf,
  fitSun,
  geometry,
  hitTestSunburst,
  indexOfIdSun,
  interpolateSun,
  layoutSunburst,
  neighborSun,
  visibleArcs,
} from "../src/lib/sunburst.ts";
import type { TreemapInput } from "../src/lib/treemap.ts";

let nextId = 1;

function file(size: number, category: TreemapInput["category"] = "other"): TreemapInput {
  nextId += 1;
  return { id: nextId, name: `f${nextId}`, size, category };
}

function dir(children: TreemapInput[], remainingBytes = 0): TreemapInput {
  nextId += 1;
  const size = children.reduce((sum, child) => sum + child.size, 0) + remainingBytes;
  return {
    id: nextId,
    name: `d${nextId}`,
    size,
    category: "other",
    children,
    remainingCount: remainingBytes > 0 ? 1 : 0,
    remainingBytes,
  };
}

const W = 800;
const H = 600;

test("spans are fractions of the circle, biggest first, children inside parents", () => {
  const small = file(100);
  const big = file(300);
  const folder = dir([file(200), file(200)]);
  const root = dir([small, folder, big]);
  const layout = layoutSunburst(root);

  assert.equal(layout.ids[0], root.id, "the root is the centre");
  assert.equal(layout.x0[0], 0);
  assert.equal(layout.x1[0], 1);
  assert.equal(layout.maxDepth, 2);

  // Children of the root, in angular order: folder (400), big (300), small (100).
  const ring = [1, 2, 3].map((index) => layout.ids[index]);
  assert.deepEqual(ring, [folder.id, big.id, small.id]);
  const f = indexOfIdSun(layout, folder.id) ?? -1;
  assert.ok(Math.abs((layout.x1[f] ?? 0) - (layout.x0[f] ?? 0) - 0.5) < 1e-9);
  const start = layout.childStart[f] ?? -1;
  for (let child = start; child < start + (layout.childCount[f] ?? 0); child += 1) {
    assert.equal(layout.parent[child], f);
    assert.ok((layout.x0[child] ?? 0) >= (layout.x0[f] ?? 0) - 1e-9);
    assert.ok((layout.x1[child] ?? 0) <= (layout.x1[f] ?? 0) + 1e-9);
  }
});

test("bytes the backend did not send are a rest arc at the end of the span", () => {
  const root = dir([file(600)], 400);
  const layout = layoutSunburst(root);
  assert.equal(layout.rest.length, 3);
  const [x0, x1, depth] = layout.rest;
  assert.ok(Math.abs((x0 ?? 0) - 0.6) < 1e-9);
  assert.equal(x1, 1);
  assert.equal(depth, 1);
});

test("hit-testing finds the arc under the point, and the centre is the root", () => {
  const left = file(500);
  const right = file(500);
  const layout = layoutSunburst(dir([left, right]));
  const geo = geometry(layout, FULL_VIEW, W, H);
  const drawn = visibleArcs(layout, FULL_VIEW, geo);
  const ringMiddle = geo.ring * 1.5;

  // 0 rad is up, clockwise: the first half of the circle is on the right.
  const onRight = hitTestSunburst(layout, drawn, FULL_VIEW, geo, geo.cx + ringMiddle, geo.cy);
  const onLeft = hitTestSunburst(layout, drawn, FULL_VIEW, geo, geo.cx - ringMiddle, geo.cy);
  assert.equal(layout.ids[onRight ?? -1], left.id);
  assert.equal(layout.ids[onLeft ?? -1], right.id);
  assert.equal(hitTestSunburst(layout, drawn, FULL_VIEW, geo, geo.cx, geo.cy), 0);
  assert.equal(hitTestSunburst(layout, drawn, FULL_VIEW, geo, 0, 0), null, "outside the circle");
});

test("zooming fits an arc to the whole circle with it at the centre", () => {
  const target = dir([file(50), file(50)]);
  const layout = layoutSunburst(dir([target, file(900)]));
  const index = indexOfIdSun(layout, target.id) ?? -1;
  const view = fitSun(layout, index);
  const geo = geometry(layout, view, W, H);
  const arc = arcOf(layout, index, view, geo);
  assert.ok(arc !== null);
  assert.equal(arc.inner, 0);
  assert.ok(Math.abs(arc.end - arc.start - 2 * Math.PI) < 1e-9);
  // The parent is no longer drawn; the target's children fill the first ring.
  assert.ok(!visibleArcs(layout, view, geo).includes(0));

  const halfway = interpolateSun(FULL_VIEW, view, 0.5);
  assert.ok(halfway.x0 > 0 && halfway.x0 < view.x0 + 1e-9 && halfway.d0 === 0.5);
  assert.deepEqual(interpolateSun(FULL_VIEW, view, 1), view);
});

test("hover highlights the path from the centre", () => {
  const leaf = file(10);
  const inner = dir([leaf]);
  const layout = layoutSunburst(dir([dir([inner]), file(5)]));
  const index = indexOfIdSun(layout, leaf.id) ?? -1;
  const path = ancestorPath(layout, index);
  assert.equal(path.size, 4, "leaf, two folders, root");
  assert.ok(path.has(0) && path.has(index));
});

test("arrow keys go round siblings, out to children, and in to the parent", () => {
  const a = dir([file(30), file(20)]);
  const b = file(40);
  const c = file(10);
  const layout = layoutSunburst(dir([a, b, c]));
  const at = (id: number) => indexOfIdSun(layout, id);
  assert.equal(neighborSun(layout, null, "next"), at(a.id), "starts on the biggest arc");
  assert.equal(neighborSun(layout, at(a.id), "next"), at(b.id));
  assert.equal(neighborSun(layout, at(c.id), "next"), at(c.id), "stops at the last sibling");
  assert.equal(neighborSun(layout, at(b.id), "previous"), at(a.id));
  const out = neighborSun(layout, at(a.id), "out") ?? -1;
  assert.equal(layout.parent[out], at(a.id));
  assert.equal(layout.value[out], 30, "outward goes to the biggest child");
  assert.equal(neighborSun(layout, out, "in"), at(a.id));
  assert.equal(neighborSun(layout, at(a.id), "in"), 0);
});

test("1M nodes: layout once, then frames well inside the 16 ms budget", () => {
  const children: TreemapInput[] = [];
  for (let d = 0; d < 1000; d += 1) {
    const leaves: TreemapInput[] = [];
    for (let f = 0; f < 1000; f += 1) {
      leaves.push(file(1 + ((d * 7919 + f * 104_729) % 1_000_000)));
    }
    children.push(dir(leaves));
  }
  const root = dir(children);
  const started = performance.now();
  const layout = layoutSunburst(root);
  const layoutMs = performance.now() - started;
  assert.equal(layout.count, 1_001_001);

  // A zoom from the whole circle into one folder, as a transition draws it.
  const target = fitSun(layout, 1);
  const frames = 60;
  let worst = 0;
  const sweep = performance.now();
  for (let step = 0; step <= frames; step += 1) {
    const view = interpolateSun(FULL_VIEW, target, step / frames);
    const begin = performance.now();
    const geo = geometry(layout, view, 1600, 1000);
    const drawn = visibleArcs(layout, view, geo);
    worst = Math.max(worst, performance.now() - begin);
    assert.ok(drawn.length <= MAX_ARCS);
    assert.ok(drawn.length > 0);
  }
  const average = (performance.now() - sweep) / (frames + 1);
  console.log(
    `sunburst 1M nodes: layout ${layoutMs.toFixed(0)} ms, frame avg ${average.toFixed(2)} ms, worst ${worst.toFixed(2)} ms`,
  );
  assert.ok(worst < 16, `worst frame ${worst.toFixed(2)} ms`);
});
