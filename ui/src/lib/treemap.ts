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
