// The Overview screen's arithmetic. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type {
  CategoryDto,
  RiskLevel,
  RuleGroupDto,
  VolumeDto,
} from "../src/lib/api.ts";
import {
  capacitySegments,
  categorySegments,
  reclaimSummary,
  topWins,
} from "../src/lib/overview.ts";

const GB = 1024 ** 3;

function volume(total: number, available: number, purgeable = 0): VolumeDto {
  return {
    name: "Macintosh HD",
    mountPoint: "/",
    total,
    available,
    used: total - available,
    purgeable,
  };
}

function sum(segments: readonly { bytes: number }[]): number {
  return segments.reduce((total, segment) => total + segment.bytes, 0);
}

test("gauge segments add up to the volume's capacity", () => {
  const gauge = capacitySegments(volume(500 * GB, 120 * GB, 20 * GB), 300 * GB);
  assert.equal(sum(gauge.segments), 500 * GB);
  assert.equal(gauge.total, 500 * GB);
  assert.deepEqual(
    gauge.segments.map((segment) => [segment.key, segment.bytes / GB]),
    [
      ["scan", 300],
      ["other", 80],
      ["purgeable", 20],
      ["free", 100],
    ],
  );
  assert.equal(gauge.clamped, false);
  const fractions = gauge.segments.reduce(
    (total, segment) => total + segment.fraction,
    0,
  );
  assert.ok(Math.abs(fractions - 1) < 1e-12);
});

test("a scan larger than used space is clamped, and says so", () => {
  const gauge = capacitySegments(volume(100 * GB, 40 * GB), 70 * GB);
  assert.equal(sum(gauge.segments), 100 * GB);
  assert.equal(gauge.segments[0]?.bytes, 60 * GB);
  assert.equal(gauge.segments[1]?.bytes, 0);
  assert.equal(gauge.clamped, true);
});

test("a nonsense volume still yields a consistent gauge", () => {
  const gauge = capacitySegments(volume(0, 10, 50), 5);
  assert.equal(sum(gauge.segments), 0);
  assert.ok(gauge.segments.every((segment) => segment.fraction === 0));
});

function category(name: CategoryDto["category"], bytes: number): CategoryDto {
  return { category: name, label: name, bytesAlloc: bytes, fileCount: 1 };
}

test("category segments add up to the scan total, biggest first", () => {
  const categories = [
    category("images", 30),
    category("video", 50),
    category("other", 20),
  ];
  const segments = categorySegments(categories, 100);
  assert.equal(sum(segments), 100);
  assert.deepEqual(
    segments.map((segment) => segment.key),
    ["video", "images", "other"],
  );
});

test("a gap between categories and the total is shown, not hidden", () => {
  const segments = categorySegments(
    [category("images", 30), category("code", 0)],
    100,
  );
  assert.equal(sum(segments), 100);
  assert.equal(segments.at(-1)?.key, "unaccounted");
  assert.equal(segments.at(-1)?.bytes, 70);
  assert.ok(
    !segments.some((segment) => segment.key === "code"),
    "empty categories are skipped",
  );
});

function rule(
  id: string,
  risk: RiskLevel,
  bytes: number,
  deletable = true,
): RuleGroupDto {
  return {
    ruleId: id,
    title: id,
    kind: "regenerable",
    risk,
    explain: "",
    command: null,
    deletable,
    bytes,
    items: [],
  };
}

test("reclaim uses the engine's figures: safe × 1 + review × 0.5", () => {
  const reclaim = reclaimSummary({
    reclaimSafe: 5 * GB,
    reclaimReview: 10 * GB,
    groups: [],
  });
  assert.equal(reclaim.safe, 5 * GB);
  assert.equal(reclaim.review, 10 * GB);
  assert.equal(reclaim.score, 10 * GB);
  assert.deepEqual(
    reclaimSummary({ reclaimSafe: -1, reclaimReview: 0, groups: [] }),
    { safe: 0, review: 0, score: 0, manual: 0, manualTitles: [] },
  );
});

test("report-only safe or review cards are freed by hand, biggest first", () => {
  const reclaim = reclaimSummary({
    reclaimSafe: 0,
    reclaimReview: 0,
    groups: [
      rule("trash", "review", 2 * GB, false),
      rule("docker", "review", 15 * GB, false),
      rule("swap", "never", 2 * GB, false),
      rule("caches", "safe", 4 * GB),
      rule("snapshots", "safe", 0, false),
    ],
  });
  assert.equal(reclaim.manual, 17 * GB);
  assert.deepEqual(reclaim.manualTitles, ["docker", "trash"]);
  assert.equal(reclaim.score, 0);
});

test("wins are the biggest deletable safe or review cards", () => {
  const rules = [
    rule("caches", "safe", 4 * GB),
    rule("downloads", "review", 10 * GB),
    rule("system", "never", 50 * GB, false),
    rule("docker", "review", 60 * GB, false),
    rule("logs", "safe", 1 * GB),
  ];
  assert.deepEqual(
    topWins(rules, 2).map((win) => win.ruleId),
    ["downloads", "caches"],
  );
});
