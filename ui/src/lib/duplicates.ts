/**
 * Duplicates screen helpers (#40): the keep-one suggestion and a selection
 * that can never hold every copy of a group. Pure, so they are tested.
 *
 * The suggestion (PLAN §12) is only ever shown; nothing is selected until a
 * click asks for it, and the receipt confirms again.
 */

import type { DupFileDto, DupGroupDto } from "./api";

/** Paths chosen for the Trash, by group hash. */
export type DupSelection = ReadonlyMap<string, ReadonlySet<string>>;

/** Documents first, Downloads and the Trash last. */
function place(path: string): number {
  const lower = path.toLowerCase().replaceAll("\\", "/");
  if (/\/(\.trash|\$recycle\.bin)(\/|$)/.test(lower)) return 3;
  if (lower.includes("/downloads/")) return 2;
  if (lower.includes("/documents/")) return 0;
  return 1;
}

function depth(path: string): number {
  return path.split(/[\\/]/).length;
}

/**
 * The copy to keep: best place (Documents › elsewhere › Downloads › Trash),
 * then newest, then shallowest path, then alphabetical.
 */
export function suggestKeep(files: readonly DupFileDto[]): string | null {
  const ranked = [...files].sort(
    (a, b) =>
      place(a.path) - place(b.path) ||
      (b.modified ?? -Infinity) - (a.modified ?? -Infinity) ||
      depth(a.path) - depth(b.path) ||
      a.path.localeCompare(b.path),
  );
  return ranked[0]?.path ?? null;
}

export function selectedIn(
  selection: DupSelection,
  group: DupGroupDto,
): ReadonlySet<string> {
  return selection.get(group.hash) ?? new Set();
}

/**
 * Whether `path` may be ticked: always when already ticked (to untick), and
 * otherwise only while at least one other copy stays unticked.
 */
export function canSelect(
  selection: DupSelection,
  group: DupGroupDto,
  path: string,
): boolean {
  const chosen = selectedIn(selection, group);
  return chosen.has(path) || chosen.size + 1 < group.files.length;
}

/** Tick or untick one copy. Refuses (returns `selection`) to tick the last one. */
export function toggle(
  selection: DupSelection,
  group: DupGroupDto,
  path: string,
): DupSelection {
  if (!canSelect(selection, group, path)) {
    return selection;
  }
  const chosen = new Set(selectedIn(selection, group));
  if (!chosen.delete(path)) {
    chosen.add(path);
  }
  const next = new Map(selection);
  if (chosen.size === 0) {
    next.delete(group.hash);
  } else {
    next.set(group.hash, chosen);
  }
  return next;
}

/** Tick every copy except `keep` — what "Keep this one" does. */
export function keepOnly(
  selection: DupSelection,
  group: DupGroupDto,
  keep: string,
): DupSelection {
  const next = new Map(selection);
  next.set(
    group.hash,
    new Set(
      group.files.map((file) => file.path).filter((path) => path !== keep),
    ),
  );
  return next;
}

/** Drop selections for groups (or copies) no longer listed. */
export function prune(
  selection: DupSelection,
  groups: readonly DupGroupDto[],
): DupSelection {
  const next = new Map<string, ReadonlySet<string>>();
  for (const group of groups) {
    const paths = new Set(group.files.map((file) => file.path));
    const kept = [...selectedIn(selection, group)].filter((path) =>
      paths.has(path),
    );
    if (kept.length > 0 && kept.length < group.files.length) {
      next.set(group.hash, new Set(kept));
    }
  }
  return next;
}

/** Every ticked path, and the bytes they free. */
export function selectionTotal(
  selection: DupSelection,
  groups: readonly DupGroupDto[],
): { readonly paths: string[]; readonly bytes: number } {
  const paths: string[] = [];
  let bytes = 0;
  for (const group of groups) {
    const chosen = selectedIn(selection, group);
    for (const file of group.files) {
      if (chosen.has(file.path)) {
        paths.push(file.path);
        bytes += file.sizeAlloc;
      }
    }
  }
  return { paths, bytes };
}
