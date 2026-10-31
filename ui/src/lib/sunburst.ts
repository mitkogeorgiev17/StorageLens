/**
 * Sunburst layout and level of detail (#27), independent of React so it is
 * tested under Node. The Map's alternate view: "who owns the disk" at a glance.
 *
 * - `d3-hierarchy`'s partition gives every node an angular span in [0, 1] (a
 *   fraction of the circle) and a ring (its depth). The root is the centre disk.
 * - Bytes the backend did not send (`remainingBytes`) are the tail of their
 *   parent's span, drawn as a neutral "rest" arc so they never read as free space.
 * - A {@link SunView} is the visible angular domain plus the ring at the centre.
 *   Zooming into an arc animates the view to that arc's span; the layout itself
 *   is computed once per tree, never per frame.
 * - Level of detail: an arc's on-screen span is proportional to its bytes, so
 *   arcs are visited biggest first and the walk stops at the first one too thin
 *   to see. A frame never draws more than {@link MAX_ARCS}.
 */

import { hierarchy, partition } from "d3-hierarchy";

import type { Category } from "./api";
import { CATEGORY_COLORS, CATEGORY_ORDER, type TreemapInput } from "./treemap.ts";

/** Arcs thinner than this, in CSS px along their outer edge, are not drawn. */
export const MIN_ARC_PX = 1;
/** Hard cap on arcs drawn per frame. */
export const MAX_ARCS = 4000;
/** Space between the outermost ring and the edge of the canvas, CSS px. */
const MARGIN = 8;

const CATEGORY_INDEX = new Map<Category, number>(
  CATEGORY_ORDER.map((category, index) => [category, index]),
);

export interface SunburstLayout {
  /** Index 0 is the root. Breadth-first, so siblings are contiguous and in
   * angular (biggest-first) order. */
  readonly count: number;
  readonly x0: Float64Array;
  readonly x1: Float64Array;
  readonly depth: Uint8Array;
  /** -1 for the root. */
  readonly parent: Int32Array;
  readonly childStart: Int32Array;
  readonly childCount: Int32Array;
  /** 1 for a file or an unexpanded folder. */
  readonly leaf: Uint8Array;
  readonly category: Uint8Array;
  readonly value: Float64Array;
  readonly ids: readonly number[];
  readonly names: readonly string[];
  /** Node indices, biggest first: the level-of-detail walk order. */
  readonly order: Uint32Array;
  /** Bytes not sent by the backend: `[x0, x1, depth]` triples. */
  readonly rest: Float64Array;
  readonly maxDepth: number;
  readonly totalBytes: number;
}

/** The visible angular domain, and the depth drawn at the centre. */
export interface SunView {
  readonly x0: number;
  readonly x1: number;
  readonly d0: number;
}

export const FULL_VIEW: SunView = { x0: 0, x1: 1, d0: 0 };

function ownValue(node: TreemapInput): number {
  if (node.children !== undefined && node.children.length > 0) {
    return Math.max(0, node.remainingBytes ?? 0);
  }
  return Math.max(0, node.size);
}

export function layoutSunburst(root: TreemapInput): SunburstLayout {
  const tree = hierarchy<TreemapInput>(root, (node) =>
    node.children !== undefined && node.children.length > 0 ? node.children : null,
  )
    .sum(ownValue)
    .sort((left, right) => (right.value ?? 0) - (left.value ?? 0));
  const laidOut = partition<TreemapInput>().size([1, 1]).round(false)(tree);

  // `descendants()` is breadth-first: each node's children are contiguous.
  const nodes = laidOut.descendants();
  const count = nodes.length;
  const x0 = new Float64Array(count);
  const x1 = new Float64Array(count);
  const depth = new Uint8Array(count);
  const parent = new Int32Array(count).fill(-1);
  const childStart = new Int32Array(count).fill(-1);
  const childCount = new Int32Array(count);
  const leaf = new Uint8Array(count);
  const category = new Uint8Array(count);
  const value = new Float64Array(count);
  const ids: number[] = new Array<number>(count);
  const names: string[] = new Array<string>(count);
  const position = new Map<(typeof nodes)[number], number>();
  const rest: number[] = [];
  let maxDepth = 0;

  for (let index = 0; index < count; index += 1) {
    const node = nodes[index];
    if (node === undefined) {
      continue;
    }
    position.set(node, index);
    x0[index] = node.x0;
    x1[index] = node.x1;
    depth[index] = Math.min(255, node.depth);
    maxDepth = Math.max(maxDepth, node.depth);
    value[index] = node.value ?? 0;
    leaf[index] = node.children === undefined ? 1 : 0;
    category[index] = CATEGORY_INDEX.get(node.data.category) ?? CATEGORY_ORDER.length - 1;
    ids[index] = node.data.id;
    names[index] = node.data.name;
    if (node.parent !== null) {
      const up = position.get(node.parent) ?? -1;
      parent[index] = up;
      if (up >= 0) {
        if (childStart[up] === -1) {
          childStart[up] = index;
        }
        childCount[up] = (childCount[up] ?? 0) + 1;
      }
    }
    // The children fill the span from its start; the leftover is the rest.
    const children = node.children;
    if (children !== undefined && (node.data.remainingBytes ?? 0) > 0) {
      const last = children[children.length - 1];
      const start = last === undefined ? node.x0 : last.x1;
      if (node.x1 - start > 0) {
        rest.push(start, node.x1, node.depth + 1);
        maxDepth = Math.max(maxDepth, node.depth + 1);
      }
    }
  }

  const order = new Uint32Array(count);
  for (let index = 0; index < count; index += 1) {
    order[index] = index;
  }
  order.sort((left, right) => (value[right] ?? 0) - (value[left] ?? 0) || left - right);

  return {
    count,
    x0,
    x1,
    depth,
    parent,
    childStart,
    childCount,
    leaf,
    category,
    value,
    ids,
    names,
    order,
    rest: Float64Array.from(rest),
    maxDepth,
    totalBytes: laidOut.value ?? 0,
  };
}

/** Centre and radii for a canvas of `width` × `height` CSS px. */
export interface Geometry {
  readonly cx: number;
  readonly cy: number;
  readonly radius: number;
  /** Width of one ring at this view (rings widen as the view zooms in). */
  readonly ring: number;
}

export function geometry(layout: SunburstLayout, view: SunView, width: number, height: number): Geometry {
  const radius = Math.max(0, Math.min(width, height) / 2 - MARGIN);
  const rings = Math.max(1, layout.maxDepth - view.d0 + 1);
  return { cx: width / 2, cy: height / 2, radius, ring: radius / rings };
}

/** One arc on screen: angles in radians (0 = up, clockwise), radii in px. */
export interface Arc {
  readonly start: number;
  readonly end: number;
  readonly inner: number;
  readonly outer: number;
}

/** Where a span `[a, b]` at `depth` lands on screen; `null` when off it. */
export function spanArc(a: number, b: number, depth: number, view: SunView, geo: Geometry): Arc | null {
  const width = view.x1 - view.x0;
  if (width <= 0) {
    return null;
  }
  const t0 = Math.max(0, (a - view.x0) / width);
  const t1 = Math.min(1, (b - view.x0) / width);
  const ring = depth - view.d0;
  const outer = Math.min(geo.radius, (ring + 1) * geo.ring);
  const inner = Math.max(0, ring * geo.ring);
  if (t1 <= t0 || outer <= inner) {
    return null;
  }
  return { start: t0 * 2 * Math.PI, end: t1 * 2 * Math.PI, inner, outer };
}

export function arcOf(layout: SunburstLayout, index: number, view: SunView, geo: Geometry): Arc | null {
  return spanArc(
    layout.x0[index] ?? 0,
    layout.x1[index] ?? 0,
    layout.depth[index] ?? 0,
    view,
    geo,
  );
}

/**
 * The arcs one frame draws, biggest first. Arcs are visited in byte order and
 * the walk stops at the first one whose full span would be under
 * {@link MIN_ARC_PX}: every later arc is smaller still.
 */
export function visibleArcs(layout: SunburstLayout, view: SunView, geo: Geometry): Uint32Array {
  const drawn = new Uint32Array(Math.min(layout.count, MAX_ARCS));
  const total = layout.totalBytes;
  const width = view.x1 - view.x0;
  if (total <= 0 || width <= 0 || geo.radius <= 0) {
    return drawn.subarray(0, 0);
  }
  // Bytes below which an arc is thinner than MIN_ARC_PX at the outer edge.
  const threshold = (MIN_ARC_PX / (2 * Math.PI * geo.radius)) * width * total;
  let length = 0;
  for (let position = 0; position < layout.count && length < drawn.length; position += 1) {
    const index = layout.order[position] ?? 0;
    if ((layout.value[index] ?? 0) < threshold) {
      break;
    }
    const x0 = layout.x0[index] ?? 0;
    const x1 = layout.x1[index] ?? 0;
    const depth = layout.depth[index] ?? 0;
    if (x1 <= view.x0 || x0 >= view.x1 || depth + 1 <= view.d0) {
      continue;
    }
    drawn[length] = index;
    length += 1;
  }
  return drawn.subarray(0, length);
}

/** The drawn arc under `(x, y)` (CSS px), or `null`. */
export function hitTestSunburst(
  layout: SunburstLayout,
  drawn: Uint32Array,
  view: SunView,
  geo: Geometry,
  x: number,
  y: number,
): number | null {
  const dx = x - geo.cx;
  const dy = y - geo.cy;
  const distance = Math.hypot(dx, dy);
  if (distance > geo.radius || geo.ring <= 0) {
    return null;
  }
  // 0 at the top, clockwise, as drawn.
  let turn = (Math.atan2(dy, dx) + Math.PI / 2) / (2 * Math.PI);
  turn -= Math.floor(turn);
  const angle = view.x0 + turn * (view.x1 - view.x0);
  const ring = view.d0 + distance / geo.ring;
  for (const index of drawn) {
    const depth = layout.depth[index] ?? 0;
    if (
      ring >= depth &&
      ring < depth + 1 &&
      angle >= (layout.x0[index] ?? 0) &&
      angle < (layout.x1[index] ?? 0)
    ) {
      return index;
    }
  }
  return null;
}

export function indexOfIdSun(layout: SunburstLayout, id: number): number | null {
  const index = layout.ids.indexOf(id);
  return index === -1 ? null : index;
}

/** The view with arc `index` filling the circle and sitting at the centre. */
export function fitSun(layout: SunburstLayout, index: number): SunView {
  return {
    x0: layout.x0[index] ?? 0,
    x1: layout.x1[index] ?? 1,
    d0: layout.depth[index] ?? 0,
  };
}

export function interpolateSun(from: SunView, to: SunView, t: number): SunView {
  const k = Math.min(1, Math.max(0, t));
  return {
    x0: from.x0 + (to.x0 - from.x0) * k,
    x1: from.x1 + (to.x1 - from.x1) * k,
    d0: from.d0 + (to.d0 - from.d0) * k,
  };
}

/** `index` and its ancestors up to the root: the hovered path. */
export function ancestorPath(layout: SunburstLayout, index: number): Set<number> {
  const path = new Set<number>();
  for (let current = index; current >= 0; current = layout.parent[current] ?? -1) {
    path.add(current);
  }
  return path;
}

export type SunDirection = "previous" | "next" | "in" | "out";

/**
 * Keyboard movement: `previous` / `next` go round between siblings, `out` to
 * the biggest child, `in` to the parent. From no selection, the biggest arc of
 * the first ring.
 */
export function neighborSun(
  layout: SunburstLayout,
  from: number | null,
  direction: SunDirection,
): number | null {
  if (from === null || from < 0 || from >= layout.count) {
    return layout.count > 1 ? 1 : null;
  }
  const up = layout.parent[from] ?? -1;
  switch (direction) {
    case "in":
      return up >= 0 ? up : from;
    case "out":
      return (layout.childCount[from] ?? 0) > 0 ? (layout.childStart[from] ?? from) : from;
    case "previous":
    case "next": {
      if (up < 0) {
        return from;
      }
      const first = layout.childStart[up] ?? from;
      const last = first + (layout.childCount[up] ?? 1) - 1;
      const step = direction === "next" ? 1 : -1;
      return Math.min(last, Math.max(first, from + step));
    }
  }
}

export function sunColor(layout: SunburstLayout, index: number): string {
  const category = CATEGORY_ORDER[layout.category[index] ?? CATEGORY_ORDER.length - 1] ?? "other";
  return CATEGORY_COLORS[category];
}
