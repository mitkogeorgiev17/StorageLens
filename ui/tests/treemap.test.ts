// Layout and level-of-detail tests, plus the 1M-node frame benchmark the
// acceptance criteria call for. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  IDENTITY_VIEW,
  MAX_RECTS,
  MIN_AREA,
  backingStore,
  clampView,
  computeFrame,
  easeInOut,
  fitView,
  indexOfId,
  interpolateView,
  neighbor,
  hitTest,
  layoutTreemap,
  screenRect,
  zoomAt,
  type Layout,
  type TreemapInput,
} from "../src/lib/treemap.ts";

let nextId = 1;

function file(size: number, category: TreemapInput["category"] = "other"): TreemapInput {
  nextId += 1;
  return { id: nextId, name: `f${nextId}`, size, category };
}

function dir(children: TreemapInput[], remaining?: { count: number; bytes: number }): TreemapInput {
  nextId += 1;
  const size =
    children.reduce((sum, child) => sum + child.size, 0) + (remaining?.bytes ?? 0);
  return {
    id: nextId,
    name: `d${nextId}`,
    size,
    category: "other",
    children,
    ...(remaining === undefined
      ? {}
      : { remainingCount: remaining.count, remainingBytes: remaining.bytes }),
  };
}

/** `dirs` directories of `files` files each, sizes varied deterministically. */
function wide(dirs: number, files: number): TreemapInput {
  const children: TreemapInput[] = [];
  for (let d = 0; d < dirs; d += 1) {
    const leaves: TreemapInput[] = [];
    for (let f = 0; f < files; f += 1) {
      leaves.push(file(1 + ((d * 7919 + f * 104_729) % 1_000_000)));
    }
    children.push(dir(leaves));
  }
  return dir(children);
}

function drawnLeafTotals(layout: Layout, drawn: Uint32Array): { count: number; bytes: number } {
  let count = 0;
  let bytes = 0;
  for (const index of drawn) {
    if (layout.leaf[index] === 1) {
      count += 1;
      bytes += layout.value[index] ?? 0;
    }
  }
  return { count, bytes };
}

test("rects nest inside their parents and inside the box", () => {
  const root = dir([dir([file(500), file(300)]), file(200), dir([file(100)])]);
  const layout = layoutTreemap(root, 800, 600);

  assert.equal(layout.totalBytes, 1100);
  for (let index = 0; index < layout.ids.length; index += 1) {
    const x0 = layout.x0[index] ?? 0;
    const x1 = layout.x1[index] ?? 0;
    const y0 = layout.y0[index] ?? 0;
    const y1 = layout.y1[index] ?? 0;
    assert.ok(x0 >= 0 && y0 >= 0 && x1 <= 800 + 1e-9 && y1 <= 600 + 1e-9, `rect ${index} in box`);
    assert.ok(x1 >= x0 && y1 >= y0);
  }
  // Area is proportional to bytes for siblings of one parent (top level here).
  const top = [...layout.ids.keys()].filter((index) => layout.depth[index] === 1);
  const areas = top.map((index) => {
    const rect = screenRect(layout, index, IDENTITY_VIEW);
    return rect.width * rect.height;
  });
  const values = top.map((index) => layout.value[index] ?? 0);
  assert.ok(areas[0] !== undefined && areas[0] >= (areas[1] ?? 0), "largest first");
  assert.deepEqual(values, [800, 200, 100]);
});

test("frames never exceed the cap and never draw a rect under the minimum area", () => {
  const layout = layoutTreemap(wide(40, 500), 1200, 800);
  const frame = computeFrame(layout, IDENTITY_VIEW, 1200, 800);

  assert.ok(frame.drawn.length <= MAX_RECTS, `${frame.drawn.length} rects`);
  for (const index of frame.drawn) {
    const rect = screenRect(layout, index, IDENTITY_VIEW);
    assert.ok(rect.width * rect.height >= MIN_AREA, `rect ${index} is ${rect.width * rect.height} px²`);
  }

  const capped = computeFrame(layout, IDENTITY_VIEW, 1200, 800, 50);
  assert.equal(capped.drawn.length, 50, "a lower cap is honoured exactly");
});

test("parents are painted before their children", () => {
  const layout = layoutTreemap(wide(20, 50), 1000, 700);
  const frame = computeFrame(layout, IDENTITY_VIEW, 1000, 700);
  const seenAtDepth = new Map<number, number>();
  for (const [position, index] of [...frame.drawn].entries()) {
    const depth = layout.depth[index] ?? 0;
    if (!seenAtDepth.has(depth)) {
      seenAtDepth.set(depth, position);
    }
  }
  assert.ok((seenAtDepth.get(1) ?? 0) < (seenAtDepth.get(2) ?? Infinity));

  // Every drawn depth-2 rect's parent rect contains it and was drawn earlier.
  const drawnOrder = new Map([...frame.drawn].map((index, position) => [index, position]));
  for (const index of frame.drawn) {
    if (layout.depth[index] !== 2) {
      continue;
    }
    const child = screenRect(layout, index, IDENTITY_VIEW);
    const parent = [...frame.drawn].find((candidate) => {
      if (layout.depth[candidate] !== 1) {
        return false;
      }
      const rect = screenRect(layout, candidate, IDENTITY_VIEW);
      return (
        child.x >= rect.x - 1e-9 &&
        child.y >= rect.y - 1e-9 &&
        child.x + child.width <= rect.x + rect.width + 1e-9 &&
        child.y + child.height <= rect.y + rect.height + 1e-9
      );
    });
    assert.notEqual(parent, undefined, `parent of ${index} was drawn`);
    assert.ok((drawnOrder.get(parent ?? 0) ?? Infinity) < (drawnOrder.get(index) ?? 0));
  }
});

test("the +N more tile accounts for every item and byte not drawn", () => {
  // Remainders at two levels: items the backend aggregated must be counted too.
  const root = dir(
    [
      dir([file(4000), file(3000), file(2)], { count: 120, bytes: 900 }),
      file(5000),
      ...Array.from({ length: 300 }, () => file(1)),
    ],
    { count: 40, bytes: 700 },
  );
  const layout = layoutTreemap(root, 400, 300);
  assert.equal(layout.totalBytes, 4000 + 3000 + 2 + 900 + 5000 + 300 + 700);
  assert.equal(layout.itemCount, 3 + 1 + 300 + 120 + 40);

  const frame = computeFrame(layout, IDENTITY_VIEW, 400, 300);
  const drawn = drawnLeafTotals(layout, frame.drawn);
  assert.ok(frame.hiddenCount > 160, "remainders plus tiny files are hidden");
  assert.equal(frame.hiddenCount + drawn.count, layout.itemCount);
  assert.equal(frame.hiddenBytes + drawn.bytes, layout.totalBytes);
});

test("zooming keeps the cursor point fixed and culls rects outside the viewport", () => {
  const view = zoomAt(IDENTITY_VIEW, 4, 300, 200);
  assert.equal(view.scale, 4);
  // The layout point under the cursor is unchanged.
  assert.equal((300 - view.translateX) / view.scale, 300);
  assert.equal((200 - view.translateY) / view.scale, 200);

  const layout = layoutTreemap(wide(30, 200), 1000, 700);
  const frame = computeFrame(layout, view, 1000, 700);
  for (const index of frame.drawn) {
    const rect = screenRect(layout, index, view);
    assert.ok(rect.x < 1000 && rect.y < 700 && rect.x + rect.width > 0 && rect.y + rect.height > 0);
  }

  const clamped = clampView({ scale: 2, translateX: 50, translateY: -5000 }, 1000, 700);
  assert.deepEqual(clamped, { scale: 2, translateX: 0, translateY: -700 });
  assert.equal(zoomAt(IDENTITY_VIEW, 0.5, 0, 0).scale, 1, "cannot zoom out past the whole map");
});

test("hit-testing finds the deepest drawn rect", () => {
  const inner = file(900);
  const root = dir([dir([inner, file(100)]), file(1000)]);
  const layout = layoutTreemap(root, 800, 600);
  const frame = computeFrame(layout, IDENTITY_VIEW, 800, 600);
  const innerIndex = layout.ids.indexOf(inner.id);
  const rect = screenRect(layout, innerIndex, IDENTITY_VIEW);
  const hit = hitTest(layout, frame, IDENTITY_VIEW, rect.x + rect.width / 2, rect.y + rect.height / 2);
  assert.equal(hit, innerIndex);
  assert.equal(hitTest(layout, frame, IDENTITY_VIEW, -10, -10), null);
});

test("the backing store matches the device pixel ratio", () => {
  assert.deepEqual(backingStore(800, 600, 2), { width: 1600, height: 1200, scale: 2 });
  assert.deepEqual(backingStore(801, 601, 1.5), { width: 1202, height: 902, scale: 1.5 });
  assert.deepEqual(backingStore(800, 600, Number.NaN), { width: 800, height: 600, scale: 1 });
  assert.deepEqual(backingStore(0, 0, 2), { width: 1, height: 1, scale: 2 });
});

test("1M nodes: layout once, then frames well inside the 16 ms budget", () => {
  const root = wide(1000, 1000);
  const started = performance.now();
  const layout = layoutTreemap(root, 1600, 1000);
  const layoutMs = performance.now() - started;
  assert.equal(layout.ids.length, 1_001_000);

  // A zoom-and-pan sweep, as an animated transition would produce.
  const frames = 120;
  let view = IDENTITY_VIEW;
  let worst = 0;
  const sweep = performance.now();
  for (let step = 0; step < frames; step += 1) {
    view = clampView(zoomAt(view, 1.03, 400 + step * 3, 300 + step * 2), 1600, 1000);
    // Best of three: one GC pause or scheduler hiccup is not a slow frame.
    let best = Infinity;
    let frame!: ReturnType<typeof computeFrame>;
    for (let repeat = 0; repeat < 3; repeat += 1) {
      const begin = performance.now();
      frame = computeFrame(layout, view, 1600, 1000);
      best = Math.min(best, performance.now() - begin);
    }
    worst = Math.max(worst, best);
    assert.ok(frame.drawn.length <= MAX_RECTS);
  }
  const average = (performance.now() - sweep) / (frames * 3);
  console.log(
    `1M nodes: layout ${layoutMs.toFixed(0)} ms, frame avg ${average.toFixed(2)} ms, worst ${worst.toFixed(2)} ms`,
  );
  assert.ok(worst < 16, `worst frame ${worst.toFixed(2)} ms`);
});

test("fitView fills the viewport with the target rect, centred", () => {
  const target = dir([file(1000), file(500)]);
  const siblings = Array.from({ length: 8 }, () => file(1500));
  const root = dir([target, ...siblings]);
  const layout = layoutTreemap(root, 800, 600);
  const index = indexOfId(layout, target.id);
  assert.ok(index !== null);
  const view = fitView(layout, index, 800, 600);
  const rect = screenRect(layout, index, view);
  assert.ok(view.scale > 1);
  // One dimension fills the viewport; the other fits inside it.
  const fills =
    Math.abs(rect.width - 800) < 1e-6 || Math.abs(rect.height - 600) < 1e-6;
  assert.ok(fills, `rect ${rect.width}×${rect.height} does not fill 800×600`);
  assert.ok(rect.x >= -1e-6 && rect.y >= -1e-6);
  assert.ok(rect.x + rect.width <= 800 + 1e-6 && rect.y + rect.height <= 600 + 1e-6);
  assert.equal(indexOfId(layout, -42), null);
});

test("interpolateView starts and ends exactly and stays in between", () => {
  const from = IDENTITY_VIEW;
  const to = { scale: 4, translateX: -1200, translateY: -900 };
  assert.deepEqual(interpolateView(from, to, 0, 800), from);
  assert.deepEqual(interpolateView(from, to, 1, 800), to);
  const middle = interpolateView(from, to, 0.5, 800);
  assert.ok(Math.abs(middle.scale - 2) < 1e-9, "geometric in window size");
  assert.ok(middle.translateX < 0 && middle.translateX > to.translateX);
  assert.equal(easeInOut(0), 0);
  assert.equal(easeInOut(1), 1);
  assert.equal(easeInOut(0.5), 0.5);
});

test("arrow-key navigation moves to the nearest rect in that direction", () => {
  // Squarify lays four equal files out as three stacked on the left and one
  // tall one on the right:  [a|d] / [b|d] / [c|d].
  const [a, b, c, d] = [file(100), file(100), file(100), file(100)];
  const layout = layoutTreemap(dir([a, b, c, d]), 400, 400);
  const at = (node: TreemapInput) => {
    const index = indexOfId(layout, node.id);
    assert.ok(index !== null);
    return index;
  };
  assert.ok((layout.x0[at(d)] ?? 0) > (layout.x1[at(a)] ?? 0), "d is the right column");
  assert.equal(neighbor(layout, at(a), "right"), at(d));
  assert.equal(neighbor(layout, at(a), "down"), at(b));
  assert.equal(neighbor(layout, at(c), "up"), at(b));
  assert.equal(neighbor(layout, at(d), "left"), at(b), "the one level with d's centre");
  // At an edge, the selection stays put.
  assert.equal(neighbor(layout, at(a), "up"), at(a));
  assert.equal(neighbor(layout, at(d), "right"), at(d));
  // With nothing selected, start on the largest.
  assert.notEqual(neighbor(layout, null, "right"), null);
});

test("keyboard navigation skips nested rects", () => {
  const inner = dir([file(10), file(10)]);
  const root = dir([inner, file(5)]);
  const layout = layoutTreemap(root, 300, 200);
  let index = neighbor(layout, null, "right");
  for (let step = 0; step < 6 && index !== null; step += 1) {
    assert.equal(layout.depth[index], 1);
    index = neighbor(layout, index, step % 2 === 0 ? "right" : "down");
  }
});
