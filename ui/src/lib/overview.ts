/**
 * The Overview screen's arithmetic (PLAN §10, §13), independent of React so it
 * is tested under Node. Every function returns segments whose bytes add up to
 * a stated total: a bar never silently drops or invents space.
 */

import type { CategoryDto, RuleGroupDto, VolumeDto } from "./api";

export interface Segment {
  readonly key: string;
  readonly label: string;
  readonly bytes: number;
  /** `bytes / total`, 0 when the total is 0. */
  readonly fraction: number;
}

function segment(key: string, label: string, bytes: number, total: number): Segment {
  return { key, label, bytes, fraction: total > 0 ? bytes / total : 0 };
}

/**
 * The capacity gauge: this scan, other used space, purgeable, free. Sums to
 * the volume's total.
 *
 * `scanned` is clamped to `used`: a scan can count more than the volume reports
 * in use (APFS clones and snapshots share blocks), and the gauge must still add
 * up. `clamped` says when that happened.
 */
export function capacitySegments(
  volume: VolumeDto,
  scanned: number,
): { readonly segments: readonly Segment[]; readonly total: number; readonly clamped: boolean } {
  const total = Math.max(0, volume.total);
  const available = Math.min(total, Math.max(0, volume.available));
  const used = total - available;
  const thisScan = Math.min(used, Math.max(0, scanned));
  const purgeable = Math.min(available, Math.max(0, volume.purgeable));
  return {
    total,
    clamped: scanned > used,
    segments: [
      segment("scan", "This scan", thisScan, total),
      segment("other", "Other used", used - thisScan, total),
      segment("purgeable", "Purgeable", purgeable, total),
      segment("free", "Free", available - purgeable, total),
    ],
  };
}

/**
 * The category bar, biggest first. Sums to `total` (the scan's allocated
 * bytes): if the categories disagree with it, the gap is shown as its own
 * "Unaccounted" segment rather than stretched away.
 */
export function categorySegments(
  categories: readonly CategoryDto[],
  total: number,
): readonly Segment[] {
  const segments = categories
    .filter((category) => category.bytesAlloc > 0)
    .sort((left, right) => right.bytesAlloc - left.bytesAlloc)
    .map((category) => segment(category.category, category.label, category.bytesAlloc, total));
  const counted = segments.reduce((sum, item) => sum + item.bytes, 0);
  const gap = total - counted;
  return gap > 0 ? [...segments, segment("unaccounted", "Unaccounted", gap, total)] : segments;
}

/** The reclaim headline, split as PLAN §10 asks: safe vs needs review. */
export interface Reclaim {
  readonly safe: number;
  readonly review: number;
  /** PLAN §10's score: safe × 1.0 + review × 0.5. */
  readonly score: number;
}

export function reclaimSummary(rules: readonly RuleGroupDto[]): Reclaim {
  let safe = 0;
  let review = 0;
  for (const rule of rules) {
    if (rule.risk === "safe") {
      safe += Math.max(0, rule.bytes);
    } else if (rule.risk === "review") {
      review += Math.max(0, rule.bytes);
    }
  }
  return { safe, review, score: safe + review * 0.5 };
}

/** The biggest actionable rule groups. "never" rules are not wins. */
export function topWins(rules: readonly RuleGroupDto[], count = 5): readonly RuleGroupDto[] {
  return rules
    .filter((rule) => (rule.risk === "safe" || rule.risk === "review") && rule.bytes > 0)
    .sort((left, right) => right.bytes - left.bytes)
    .slice(0, count);
}
