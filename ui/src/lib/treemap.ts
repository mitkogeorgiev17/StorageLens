/**
 * Treemap layout and level of detail (PLAN §13), independent of React and the
 * canvas so it is benchmarked and tested under Node.
 *
 * # Two phases
 *
 * 1. {@link layoutTreemap} — expensive, run only when the data or the canvas size
 *    changes. A squarified `d3-hierarchy` layout, flattened into typed arrays and
 *    an index sorted by area, largest first.
 * 2. {@link computeFrame} — cheap, run every frame. Applies the pan/zoom
 *    {@link View} to the cached rects and picks what to draw: nothing under
 *    {@link MIN_AREA} px², never more than {@link MAX_RECTS}. Because the index is
 *    sorted by area and the view scales uniformly, the walk stops at the first
 *    rect that is too small: its cost is bounded by what is drawn, not by the
 *    size of the tree.
 *
 * # What counts as an item
 *
 * Leaves are items, and so is each directory's "remaining" group — the
 * children the backend aggregated instead of sending (`remainingCount`,
 * `remainingBytes`). A remainder gets its own area inside its directory, so the
 * layout never loses bytes and the "+N more" tile reports exactly what was not
 * drawn individually: `hiddenCount = items − drawn leaves`,
 * `hiddenBytes = total − drawn leaf bytes`.
 *
 * # Draw order
 *
 * Rects are ordered by area descending, ties broken by depth ascending. A child
 * lies inside its parent, so it is never larger: parents are always visited, and
 * painted, before their children.
 */

import { hierarchy, treemap, treemapSquarify } from "d3-hierarchy";

import type { Category } from "./api";

/** Rects below this many CSS px² are not drawn individually. */
export const MIN_AREA = 6;

/** Hard cap on rects drawn per frame. */
export const MAX_RECTS = 1500;

/** Gap between siblings, CSS px. */
const PADDING_INNER = 1;

/** Room for a directory's label above its children, when it is big enough. */
const HEADER_HEIGHT = 16;
const HEADER_MIN_SIZE = 48;

/** One node of the input tree. */
export interface TreemapInput {
  readonly id: number;
  readonly name: string;
  /** Allocated bytes: own size for a file, subtree total for a directory. */
  readonly size: number;
  readonly category: Category;
  /** Present for a directory whose children were loaded. */
  readonly children?: readonly TreemapInput[];
  /** Children not sent by the backend, aggregated. */
  readonly remainingCount?: number;
  readonly remainingBytes?: number;
}

/** Stable category order, for the per-rect category index. */
export const CATEGORY_ORDER: readonly Category[] = [
  "images",
  "video",
  "audio",
  "documents",
  "spreadsheets",
  "presentations",
  "archives",
  "code",
  "data",
  "fonts",
  "three-d",
  "apps",
  "disk-images",
  "other",
];

const CATEGORY_INDEX: ReadonlyMap<Category, number> = new Map(
  CATEGORY_ORDER.map((category, index) => [category, index]),
);

/** A laid-out tree, flattened. Index `i` addresses every array. */
export interface Layout {
  readonly width: number;
  readonly height: number;
  /** Rects in layout coordinates (CSS px at identity view). */
  readonly x0: Float64Array;
  readonly y0: Float64Array;
  readonly x1: Float64Array;
  readonly y1: Float64Array;
  /** Bytes the rect represents (subtree total for a directory). */
  readonly value: Float64Array;
  readonly depth: Uint16Array;
  /** 1 for an item drawn as a leaf, 0 for a directory. */
  readonly leaf: Uint8Array;
  /** Index into {@link CATEGORY_ORDER}. */
  readonly category: Uint8Array;
  /** Input ids and names, for hit-testing and labels. */
  readonly ids: readonly number[];
  readonly names: readonly string[];
  /** Rect indices, largest area first (ties: shallower first). */
  readonly order: Uint32Array;
  /** All items: leaves plus every directory's remaining children. */
  readonly itemCount: number;
  /** Total bytes of the root. */
  readonly totalBytes: number;
}

/** Pan/zoom applied at draw time: `screen = layout × scale + translate`. */
export interface View {
  readonly scale: number;
  readonly translateX: number;
  readonly translateY: number;
}

export const IDENTITY_VIEW: View = { scale: 1, translateX: 0, translateY: 0 };

/** What one frame draws. */
export interface Frame {
  /** Rect indices to draw, in paint order. */
  readonly drawn: Uint32Array;
  /** Items not drawn individually, and their bytes. */
  readonly hiddenCount: number;
  readonly hiddenBytes: number;
}

function ownValue(node: TreemapInput): number {
  if (node.children !== undefined && node.children.length > 0) {
    return Math.max(0, node.remainingBytes ?? 0);
  }
  return Math.max(0, node.size);
}

/**
 * Lay `root` out in a `width` × `height` box. The root itself is the box and is
 * not a rect; its descendants are.
 */
export function layoutTreemap(root: TreemapInput, width: number, height: number): Layout {
  const tree = hierarchy<TreemapInput>(root, (node) =>
    node.children !== undefined && node.children.length > 0 ? node.children : null,
  )
    .sum(ownValue)
    .sort((left, right) => (right.value ?? 0) - (left.value ?? 0));

  const laidOut = treemap<TreemapInput>()
    .tile(treemapSquarify)
    .size([Math.max(0, width), Math.max(0, height)])
    .paddingInner(PADDING_INNER)
    .paddingTop((node) =>
      node.depth > 0 &&
      node.children !== undefined &&
      node.x1 - node.x0 >= HEADER_MIN_SIZE &&
      node.y1 - node.y0 >= HEADER_MIN_SIZE
        ? HEADER_HEIGHT
        : 0,
    )
    .round(false)(tree);

  const nodes = laidOut.descendants().slice(1);
  const count = nodes.length;
  const x0 = new Float64Array(count);
  const y0 = new Float64Array(count);
  const x1 = new Float64Array(count);
  const y1 = new Float64Array(count);
  const value = new Float64Array(count);
  const depth = new Uint16Array(count);
  const leaf = new Uint8Array(count);
  const category = new Uint8Array(count);
  const ids: number[] = new Array<number>(count);
  const names: string[] = new Array<string>(count);
  let itemCount = Math.max(0, root.remainingCount ?? 0);

  for (let index = 0; index < count; index += 1) {
    const node = nodes[index];
    if (node === undefined) {
      continue;
    }
    x0[index] = node.x0;
    y0[index] = node.y0;
    x1[index] = node.x1;
    y1[index] = node.y1;
    value[index] = node.value ?? 0;
    depth[index] = node.depth;
    const isLeaf = node.children === undefined;
    leaf[index] = isLeaf ? 1 : 0;
    category[index] = CATEGORY_INDEX.get(node.data.category) ?? CATEGORY_ORDER.length - 1;
    ids[index] = node.data.id;
    names[index] = node.data.name;
    itemCount += isLeaf ? 1 : Math.max(0, node.data.remainingCount ?? 0);
  }

  const order = new Uint32Array(count);
  for (let index = 0; index < count; index += 1) {
    order[index] = index;
  }
  const area = (index: number) =>
    ((x1[index] ?? 0) - (x0[index] ?? 0)) * ((y1[index] ?? 0) - (y0[index] ?? 0));
  order.sort((left, right) => area(right) - area(left) || (depth[left] ?? 0) - (depth[right] ?? 0));

  return {
    width,
    height,
    x0,
    y0,
    x1,
    y1,
    value,
    depth,
    leaf,
    category,
    ids,
    names,
    order,
    itemCount,
    totalBytes: laidOut.value ?? 0,
  };
}

/** A rect in screen coordinates. */
export interface ScreenRect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export function screenRect(layout: Layout, index: number, view: View): ScreenRect {
  const x = (layout.x0[index] ?? 0) * view.scale + view.translateX;
  const y = (layout.y0[index] ?? 0) * view.scale + view.translateY;
  return {
    x,
    y,
    width: ((layout.x1[index] ?? 0) - (layout.x0[index] ?? 0)) * view.scale,
    height: ((layout.y1[index] ?? 0) - (layout.y0[index] ?? 0)) * view.scale,
  };
}

/**
 * Choose what to draw for one frame. `viewportWidth`/`viewportHeight` are the
 * canvas size in CSS px; rects entirely outside it are skipped.
 */
export function computeFrame(
  layout: Layout,
  view: View,
  viewportWidth: number,
  viewportHeight: number,
  maxRects: number = MAX_RECTS,
  minArea: number = MIN_AREA,
): Frame {
  const drawn = new Uint32Array(Math.min(maxRects, layout.order.length));
  let count = 0;
  let drawnLeaves = 0;
  let drawnLeafBytes = 0;
  const scaleSquared = view.scale * view.scale;

  for (let position = 0; position < layout.order.length && count < drawn.length; position += 1) {
    const index = layout.order[position] ?? 0;
    const width = (layout.x1[index] ?? 0) - (layout.x0[index] ?? 0);
    const height = (layout.y1[index] ?? 0) - (layout.y0[index] ?? 0);
    // Sorted by area and scaled uniformly: everything after this is smaller.
    if (width * height * scaleSquared < minArea) {
      break;
    }

    const left = (layout.x0[index] ?? 0) * view.scale + view.translateX;
    const top = (layout.y0[index] ?? 0) * view.scale + view.translateY;
    const right = left + width * view.scale;
    const bottom = top + height * view.scale;
    if (right <= 0 || bottom <= 0 || left >= viewportWidth || top >= viewportHeight) {
      continue;
    }

    drawn[count] = index;
    count += 1;
    if (layout.leaf[index] === 1) {
      drawnLeaves += 1;
      drawnLeafBytes += layout.value[index] ?? 0;
    }
  }

  return {
    drawn: drawn.subarray(0, count),
    hiddenCount: Math.max(0, layout.itemCount - drawnLeaves),
    hiddenBytes: Math.max(0, layout.totalBytes - drawnLeafBytes),
  };
}

/** The topmost drawn rect under a screen point, or `null`. */
export function hitTest(layout: Layout, frame: Frame, view: View, x: number, y: number): number | null {
  for (let position = frame.drawn.length - 1; position >= 0; position -= 1) {
    const index = frame.drawn[position] ?? 0;
    const rect = screenRect(layout, index, view);
    if (x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height) {
      return index;
    }
  }
  return null;
}

/** Zoom by `factor` around a screen point, keeping that point fixed. */
export function zoomAt(view: View, factor: number, x: number, y: number, limits = { min: 1, max: 64 }): View {
  const scale = Math.min(limits.max, Math.max(limits.min, view.scale * factor));
  const applied = scale / view.scale;
  return {
    scale,
    translateX: x - (x - view.translateX) * applied,
    translateY: y - (y - view.translateY) * applied,
  };
}

/** Keep the zoomed map covering the viewport: no empty margin when panning. */
export function clampView(view: View, width: number, height: number): View {
  const minX = width - width * view.scale;
  const minY = height - height * view.scale;
  return {
    scale: view.scale,
    translateX: Math.min(0, Math.max(minX, view.translateX)),
    translateY: Math.min(0, Math.max(minY, view.translateY)),
  };
}

/**
 * Fill colour per category: stable, distinguishable, and mid-luminance so dark
 * labels stay readable on every tile in both themes.
 */
export const CATEGORY_COLORS: Readonly<Record<Category, string>> = {
  images: "#5b9bd5",
  video: "#e0736f",
  audio: "#b18ad8",
  documents: "#e8b44f",
  spreadsheets: "#66bb7a",
  presentations: "#f08f4f",
  archives: "#a8946b",
  code: "#4fb8b0",
  data: "#8ca0c8",
  fonts: "#d58bb5",
  "three-d": "#9cc25a",
  apps: "#7c8de0",
  "disk-images": "#c98d5f",
  other: "#9ea3ab",
};

/** Fill colour for rect `index`. */
export function colorOf(layout: Layout, index: number): string {
  const category = CATEGORY_ORDER[layout.category[index] ?? CATEGORY_ORDER.length - 1] ?? "other";
  return CATEGORY_COLORS[category];
}

/**
 * Truncate `text` to fit `width` px without measuring: system UI fonts average
 * well under 0.6 em per character, so this errs on the side of fitting.
 */
export function fitLabel(text: string, width: number, fontSize: number): string | null {
  const maxChars = Math.floor((width - 6) / (fontSize * 0.6));
  if (maxChars < 3) {
    return null;
  }
  return text.length <= maxChars ? text : `${text.slice(0, maxChars - 1)}…`;
}

/**
 * Backing-store size for a canvas shown at `cssWidth` × `cssHeight` on a
 * display with `devicePixelRatio` `dpr`. Drawing then scales by `scale` so
 * layout coordinates stay in CSS px and lines stay crisp at 2× and beyond.
 */
export function backingStore(
  cssWidth: number,
  cssHeight: number,
  dpr: number,
): { readonly width: number; readonly height: number; readonly scale: number } {
  const scale = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  return {
    width: Math.max(1, Math.round(cssWidth * scale)),
    height: Math.max(1, Math.round(cssHeight * scale)),
    scale,
  };
}

/** Display names, matching core's `Category::label`. */
export const CATEGORY_LABELS: Readonly<Record<Category, string>> = {
  images: "Images",
  video: "Video",
  audio: "Audio",
  documents: "Documents",
  spreadsheets: "Spreadsheets",
  presentations: "Presentations",
  archives: "Archives",
  code: "Code",
  data: "Data",
  fonts: "Fonts",
  "three-d": "3D / CAD",
  apps: "Apps",
  "disk-images": "Disk images",
  other: "Other",
};

// --- Navigation ----------------------------------------------------------------------

/** The rect index of input id `id`, or `null` when it was not laid out. */
export function indexOfId(layout: Layout, id: number): number | null {
  const index = layout.ids.indexOf(id);
  return index === -1 ? null : index;
}

/**
 * The view that makes rect `index` fill the viewport as far as a uniform scale
 * allows, centred. Clamped like any other view, so it never shows past the map.
 */
export function fitView(layout: Layout, index: number, width: number, height: number): View {
  const rectWidth = Math.max(1e-6, (layout.x1[index] ?? 0) - (layout.x0[index] ?? 0));
  const rectHeight = Math.max(1e-6, (layout.y1[index] ?? 0) - (layout.y0[index] ?? 0));
  const scale = Math.min(64, Math.max(1, Math.min(width / rectWidth, height / rectHeight)));
  const centreX = (layout.x0[index] ?? 0) + rectWidth / 2;
  const centreY = (layout.y0[index] ?? 0) + rectHeight / 2;
  return clampView(
    { scale, translateX: width / 2 - centreX * scale, translateY: height / 2 - centreY * scale },
    width,
    height,
  );
}

/** Smooth start and stop for transitions, `t` in [0, 1]. */
export function easeInOut(t: number): number {
  const clamped = Math.min(1, Math.max(0, t));
  return clamped < 0.5 ? 4 * clamped ** 3 : 1 - (-2 * clamped + 2) ** 3 / 2;
}

/**
 * The view `t` of the way from `from` to `to`. Interpolates the visible window
 * — its corner linearly, its size geometrically — so a zoom feels constant-speed
 * instead of rushing at the end, as interpolating the scale would.
 */
export function interpolateView(from: View, to: View, t: number, width: number): View {
  if (t <= 0) {
    return from;
  }
  if (t >= 1) {
    return to;
  }
  const fromSize = width / from.scale;
  const toSize = width / to.scale;
  const size = fromSize * (toSize / fromSize) ** t;
  const fromX = -from.translateX / from.scale;
  const fromY = -from.translateY / from.scale;
  const toX = -to.translateX / to.scale;
  const toY = -to.translateY / to.scale;
  const scale = width / size;
  return {
    scale,
    translateX: -(fromX + (toX - fromX) * t) * scale,
    translateY: -(fromY + (toY - fromY) * t) * scale,
  };
}

export type Direction = "left" | "right" | "up" | "down";

/**
 * Keyboard navigation: the top-level rect (depth 1, a child of the shown
 * folder) nearest to `from` in `direction`, or `from` itself at an edge. With
 * no current rect, the largest top-level rect.
 *
 * Candidates must lie wholly past `from`'s edge in that direction; among them
 * the closest wins, sideways offset counting double so movement stays in line.
 */
export function neighbor(layout: Layout, from: number | null, direction: Direction): number | null {
  const topLevel: number[] = [];
  for (const index of layout.order) {
    if (layout.depth[index] === 1) {
      topLevel.push(index);
    }
  }
  if (from === null || layout.depth[from] !== 1) {
    return topLevel[0] ?? null;
  }
  const edge = 0.5;
  const fx0 = layout.x0[from] ?? 0;
  const fy0 = layout.y0[from] ?? 0;
  const fx1 = layout.x1[from] ?? 0;
  const fy1 = layout.y1[from] ?? 0;
  const fcx = (fx0 + fx1) / 2;
  const fcy = (fy0 + fy1) / 2;

  let best: number | null = null;
  let bestScore = Number.POSITIVE_INFINITY;
  for (const index of topLevel) {
    if (index === from) {
      continue;
    }
    const x0 = layout.x0[index] ?? 0;
    const y0 = layout.y0[index] ?? 0;
    const x1 = layout.x1[index] ?? 0;
    const y1 = layout.y1[index] ?? 0;
    let along: number;
    let across: number;
    switch (direction) {
      case "right":
        if (x0 < fx1 - edge) continue;
        along = x0 - fx1;
        across = Math.max(0, y0 - fcy, fcy - y1);
        break;
      case "left":
        if (x1 > fx0 + edge) continue;
        along = fx0 - x1;
        across = Math.max(0, y0 - fcy, fcy - y1);
        break;
      case "down":
        if (y0 < fy1 - edge) continue;
        along = y0 - fy1;
        across = Math.max(0, x0 - fcx, fcx - x1);
        break;
      case "up":
        if (y1 > fy0 + edge) continue;
        along = fy0 - y1;
        across = Math.max(0, x0 - fcx, fcx - x1);
        break;
    }
    const score = along + 2 * across;
    if (score < bestScore) {
      bestScore = score;
      best = index;
    }
  }
  return best ?? from;
}
