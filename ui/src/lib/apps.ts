/**
 * Apps screen helpers (#38): sorting and labels, kept pure so they are tested.
 *
 * "Unknown" means there was no usage metadata. It is never sorted in among
 * "unused" rows: whatever the direction, unknown status and a missing
 * last-used date sort last.
 */

import type { AppRowDto, AppStatus } from "./api";

export type AppSortKey =
  "name" | "appBytes" | "satelliteBytes" | "totalBytes" | "lastUsed" | "status";

export interface AppSort {
  readonly key: AppSortKey;
  readonly descending: boolean;
}

export const STATUS_LABELS: Readonly<Record<AppStatus, string>> = {
  unused: "Unused",
  likely_unused: "Likely unused",
  in_use: "In use",
  unknown: "Unknown",
};

/** Ascending order of the known statuses; `unknown` is outside it. */
const STATUS_RANK: Readonly<Record<Exclude<AppStatus, "unknown">, number>> = {
  unused: 0,
  likely_unused: 1,
  in_use: 2,
};

/** The value a row sorts by; `null` always sorts last. */
function value(row: AppRowDto, key: AppSortKey): number | string | null {
  switch (key) {
    case "name":
      return row.name.toLocaleLowerCase();
    case "lastUsed":
      return row.lastUsed;
    case "status":
      return row.status === "unknown" ? null : STATUS_RANK[row.status];
    default:
      return row[key];
  }
}

export function sortApps(
  rows: readonly AppRowDto[],
  sort: AppSort,
): AppRowDto[] {
  const sign = sort.descending ? -1 : 1;
  return [...rows].sort((a, b) => {
    const left = value(a, sort.key);
    const right = value(b, sort.key);
    if (left === null || right === null) {
      if (left !== right) {
        return left === null ? 1 : -1;
      }
    } else if (left !== right) {
      return (left < right ? -1 : 1) * sign;
    }
    // Ties: biggest first, then by name, so the order is stable.
    return b.totalBytes - a.totalBytes || a.name.localeCompare(b.name);
  });
}

/** Clicking a header: same column flips, a new column starts in its natural order. */
export function nextSort(current: AppSort, key: AppSortKey): AppSort {
  if (current.key === key) {
    return { key, descending: !current.descending };
  }
  // Sizes biggest first; names A→Z; oldest use first; unused first.
  const descending =
    key === "appBytes" || key === "satelliteBytes" || key === "totalBytes";
  return { key, descending };
}

/** Why removal needs care, in words. Empty when nothing stands out. */
export function removalWarnings(row: AppRowDto): string[] {
  const warnings: string[] = [];
  if (row.running) {
    warnings.push(
      `${row.name} is running right now. Quit it first if you can.`,
    );
  }
  if (row.autostart) {
    warnings.push(
      `${row.name} has a login item, launch agent or startup entry. Remove that first, or it may fail at the next login.`,
    );
  }
  if (row.defaultHandler) {
    warnings.push(`${row.name} is the default app for some files or links.`);
  }
  return warnings;
}
