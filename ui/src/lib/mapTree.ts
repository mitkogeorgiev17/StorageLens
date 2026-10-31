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

/**
 * `base` joined with `names`, using `base`'s own separator (`\` for a Windows
 * root, `/` otherwise). With no recorded root (scans before schema v2), the
 * names alone.
 */
export function joinPath(base: string, names: readonly string[]): string {
  if (base === "") {
    return names.join("/");
  }
  const separator = base.includes("\\") && !base.includes("/") ? "\\" : "/";
  const trimmed = base.endsWith(separator) ? base.slice(0, -1) : base;
  return [trimmed, ...names].join(separator);
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
