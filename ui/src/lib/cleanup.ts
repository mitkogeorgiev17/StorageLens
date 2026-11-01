/**
 * The Cleanup screen's selection logic (#34), pure so it is tested under Node.
 *
 * A selection is a set of item paths. Only deletable cards' items can be in
 * it. Before a preview it is reduced to its top level — a selected folder
 * already covers anything selected inside it — so the receipt, the running
 * total, and the manifest all count each byte once.
 */

import type {
  ItemStatus,
  RiskLevel,
  RuleGroupDto,
  RuleKind,
  UndoOutcome,
} from "./api";

/** Over this, moving to the Trash takes a second confirmation (PLAN §15.7). */
export const CONFIRM_ABOVE = 5 * 1024 * 1024 * 1024;

export const KIND_ORDER: readonly RuleKind[] = [
  "regenerable",
  "stale",
  "duplicate",
  "leftover",
  "valuable",
  "system",
];

export const KIND_LABELS: Readonly<
  Record<RuleKind, { title: string; hint: string }>
> = {
  regenerable: {
    title: "Rebuilt automatically",
    hint: "Apps and tools recreate these when needed.",
  },
  stale: {
    title: "Untouched for a long time",
    hint: "Nothing has changed these in months.",
  },
  duplicate: {
    title: "Duplicates",
    hint: "Identical copies of files kept elsewhere.",
  },
  leftover: {
    title: "Leftovers",
    hint: "Belong to something that is no longer here.",
  },
  valuable: {
    title: "Your data — look first",
    hint: "Real data, such as backups. Never preselected.",
  },
  system: {
    title: "Managed by the system",
    hint: "Shown for the space they take; StorageLens never removes them.",
  },
};

export const RISK_LABELS: Readonly<Record<RiskLevel, string>> = {
  safe: "Safe",
  review: "Review",
  keep: "Keep",
  never: "Never",
};

export const STATUS_LABELS: Readonly<Record<ItemStatus, string>> = {
  trashed: "Moved to the Trash",
  refused: "Held back by a safety check",
  skipped: "Skipped",
  failed: "Could not be moved",
  cancelled: "Not reached: stopped",
};

export const UNDO_LABELS: Readonly<Record<UndoOutcome, string>> = {
  restored: "Put back",
  "already-restored": "Already put back",
  "not-trashed": "Was never moved",
  "not-in-trash": "No longer in the Trash",
  occupied: "Left in the Trash",
  failed: "Could not be put back",
};

export type Selection = ReadonlySet<string>;

/** What starts selected: each deletable card's preselected items. */
export function initialSelection(groups: readonly RuleGroupDto[]): Selection {
  const selected = new Set<string>();
  for (const group of groups) {
    if (group.deletable) {
      for (const item of group.items) {
        if (item.selected) {
          selected.add(item.path);
        }
      }
    }
  }
  return selected;
}

/** `selection` with `path` flipped. */
export function toggleItem(selection: Selection, path: string): Selection {
  const next = new Set(selection);
  if (!next.delete(path)) {
    next.add(path);
  }
  return next;
}

/** How much of a card is selected. */
export function groupState(
  selection: Selection,
  group: RuleGroupDto,
): "all" | "some" | "none" {
  let count = 0;
  for (const item of group.items) {
    if (selection.has(item.path)) {
      count += 1;
    }
  }
  return count === 0 ? "none" : count === group.items.length ? "all" : "some";
}

/** Select a whole card, or clear it if it is fully selected. */
export function toggleGroup(
  selection: Selection,
  group: RuleGroupDto,
): Selection {
  if (!group.deletable) {
    return selection;
  }
  const next = new Set(selection);
  const clear = groupState(selection, group) === "all";
  for (const item of group.items) {
    if (clear) {
      next.delete(item.path);
    } else {
      next.add(item.path);
    }
  }
  return next;
}

/** Every folder above `path`, nearest first (`/a/b/c` → `/a/b`, `/a`). */
function parents(path: string): string[] {
  const out: string[] = [];
  for (let at = path.length - 1; at > 0; at -= 1) {
    const char = path[at];
    if (char === "/" || char === "\\") {
      out.push(path.slice(0, at));
    }
  }
  return out;
}

/** The selected paths no other selected path contains, in a stable order. */
export function topLevel(selection: Selection): string[] {
  return [...selection]
    .filter((path) => !parents(path).some((parent) => selection.has(parent)))
    .sort();
}

/** Bytes per item path, for {@link selectionTotal}. */
export function sizeIndex(
  groups: readonly RuleGroupDto[],
): ReadonlyMap<string, number> {
  const sizes = new Map<string, number>();
  for (const group of groups) {
    for (const item of group.items) {
      sizes.set(item.path, item.bytes);
    }
  }
  return sizes;
}

/** Items and bytes the selection would move, each byte once. */
export function selectionTotal(
  selection: Selection,
  sizes: ReadonlyMap<string, number>,
): { readonly items: number; readonly bytes: number } {
  const top = topLevel(selection);
  let bytes = 0;
  for (const path of top) {
    bytes += sizes.get(path) ?? 0;
  }
  return { items: top.length, bytes };
}

/** Results by status, in a fixed order, for the summary. */
export function countBy<T extends string, I>(
  items: readonly I[],
  value: (item: I) => T,
  order: readonly T[],
): { readonly value: T; readonly count: number }[] {
  return order
    .map((wanted) => ({
      value: wanted,
      count: items.filter((item) => value(item) === wanted).length,
    }))
    .filter((entry) => entry.count > 0);
}

/**
 * macOS refused a move for lack of permission, as it does for another app's
 * protected data (an App Store app's `~/Library/Containers` folder). Full
 * Disk Access lets StorageLens move those.
 */
export function refusedForPermission(reasons: readonly (string | null)[]): boolean {
  return reasons.some(
    (reason) => reason !== null && /permission|not permitted/i.test(reason),
  );
}
