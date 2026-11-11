/**
 * The Map screen's model of the loaded tree, independent of React so it is
 * tested under Node: conversion to treemap input, an id index with parent
 * links, ancestor chains for the breadcrumb, and display paths.
 */

import type { TreeNodeDto } from "./api";
import type { TreemapInput } from "./treemap";

/** One breadcrumb step: a folder on the way from the scan root. */
export interface Crumb {
  readonly id: number;
  readonly name: string;
}

export interface IndexedNode {
  readonly node: TreeNodeDto;
  /** `null` for the loaded tree's root. */
  readonly parent: number | null;
}

export type TreeIndex = ReadonlyMap<number, IndexedNode>;

/** Levels drawn below the folder on screen (#96): its children and theirs. */
export const DRAWN_LEVELS = 2;

/**
 * An item smaller than this share of the folder on screen is folded into its
 * parent's "smaller items" tile: at a typical 1000 × 600 map that is about
 * 2,400 px², room for a name and a size.
 */
export const MIN_SHARE = 0.004;

/** At most this many tiles per folder, per level (children, grandchildren). */
export const MAX_TILES: readonly number[] = [40, 12];

/** Ids of "smaller items" tiles: far below any group id (`-(index + 1)`). */
const SMALLER_BASE = -(2 ** 40);

/** Whether `id` is a "smaller items" tile rather than an entry or a group. */
export function isSmallerItems(id: number): boolean {
  return id <= SMALLER_BASE;
}

/**
 * What the Map draws for `root` (#96): {@link DRAWN_LEVELS} levels, with every
 * item under {@link MIN_SHARE} of the root — and the backend's own remainder —
 * folded into one "N smaller items" leaf per folder. Bytes are kept: each
 * folder's children still add up to its size. Folders at the last level keep
 * `hasChildren`, so a click still opens them.
 */
export function simplifyTree(root: TreeNodeDto): TreeNodeDto {
  const threshold = root.sizeAlloc * MIN_SHARE;
  let nextId = SMALLER_BASE;
  const visit = (node: TreeNodeDto, level: number): TreeNodeDto => {
    if (node.children === null) {
      return node;
    }
    if (level >= DRAWN_LEVELS) {
      return { ...node, children: null, remainingCount: 0, remainingBytes: 0 };
    }
    const sorted = [...node.children].sort((left, right) => right.sizeAlloc - left.sizeAlloc);
    const limit = MAX_TILES[level] ?? 0;
    const kept: TreeNodeDto[] = [];
    let foldedCount = node.remainingCount;
    let foldedBytes = node.remainingBytes;
    let foldedFiles = 0;
    for (const child of sorted) {
      if (kept.length < limit && child.sizeAlloc >= threshold) {
        kept.push(visit(child, level + 1));
      } else {
        foldedCount += 1;
        foldedBytes += child.sizeAlloc;
        foldedFiles += child.fileCount;
      }
    }
    // A folder whose every item is small reads better as one solid tile.
    if (kept.length === 0 && level > 0) {
      return { ...node, children: null, remainingCount: 0, remainingBytes: 0 };
    }
    if (foldedCount > 0 && foldedBytes > 0) {
      nextId -= 1;
      kept.push({
        id: nextId,
        name: `${foldedCount.toLocaleString()} smaller ${foldedCount === 1 ? "item" : "items"}`,
        kind: "Other",
        category: "other",
        sizeAlloc: foldedBytes,
        fileCount: foldedFiles,
        hasChildren: false,
        children: null,
        remainingCount: 0,
        remainingBytes: 0,
      });
    }
    return { ...node, children: kept, remainingCount: 0, remainingBytes: 0 };
  };
  return visit(root, 0);
}

/** The backend tree as treemap input. Unexpanded directories become leaves. */
export function toTreemapInput(node: TreeNodeDto): TreemapInput {
  return {
    id: node.id,
    name: node.name,
    size: node.sizeAlloc,
    category: node.category,
    ...(node.children === null
      ? {}
      : {
          children: node.children.map(toTreemapInput),
          remainingCount: node.remainingCount,
          remainingBytes: node.remainingBytes,
        }),
  };
}

/** Every loaded node by id, with its parent. Iterative: trees can be deep. */
export function indexTree(root: TreeNodeDto): TreeIndex {
  const index = new Map<number, IndexedNode>();
  const stack: IndexedNode[] = [{ node: root, parent: null }];
  for (let entry = stack.pop(); entry !== undefined; entry = stack.pop()) {
    index.set(entry.node.id, entry);
    for (const child of entry.node.children ?? []) {
      stack.push({ node: child, parent: entry.node.id });
    }
  }
  return index;
}

/**
 * The folders from just below the loaded root down to `id`, inclusive — what
 * the breadcrumb gains when the map zooms straight into a nested folder.
 * Empty when `id` is the root or is not loaded.
 */
export function chainTo(index: TreeIndex, id: number): Crumb[] {
  const chain: Crumb[] = [];
  let current = index.get(id);
  while (current !== undefined && current.parent !== null) {
    chain.push({ id: current.node.id, name: current.node.name });
    current = index.get(current.parent);
  }
  return current === undefined ? [] : chain.reverse();
}

/** Whether zooming into `node` would show anything. */
export function canZoomInto(node: TreeNodeDto): boolean {
  return node.kind === "Dir" && node.hasChildren;
}

/** A name that is itself an absolute path: a drive, a UNC share, or a Unix root. */
const ABSOLUTE = /^([A-Za-z]:[\\/]|\\\\|\/)/;

/**
 * `base` joined with `names`, using the accumulated path's own separator (`\`
 * for a Windows root, `/` otherwise). With no recorded root (scans before
 * schema v2), the names alone.
 *
 * An absolute `name` replaces everything before it, exactly like
 * `PathBuf::push`: a multi-root scan (#195) names each root by its absolute
 * path, so the synthetic root's placeholder is dropped as soon as a real drive
 * appears.
 */
export function joinPath(base: string, names: readonly string[]): string {
  let path = base;
  for (const name of names) {
    if (path === "" || ABSOLUTE.test(name)) {
      path = name;
      continue;
    }
    const separator = path.includes("\\") && !path.includes("/") ? "\\" : "/";
    path = path.endsWith(separator) ? path + name : path + separator + name;
  }
  return path;
}

/** Display path of `id`: the breadcrumb's folders, then the loaded chain. */
export function pathOf(
  root: string,
  crumbs: readonly Crumb[],
  index: TreeIndex,
  id: number,
): string {
  const names = [...crumbs.slice(1).map((crumb) => crumb.name), ...chainTo(index, id).map((c) => c.name)];
  return joinPath(root, names);
}
