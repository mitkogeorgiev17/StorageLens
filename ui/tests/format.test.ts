// Run with `npm test` (Node's built-in runner, TypeScript via type stripping —
// no test framework dependency). Cases shared with scanbench's `human_bytes`
// tests are marked; the two must agree.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  UNKNOWN,
  formatBytes,
  formatCount,
  formatDuration,
  formatPercent,
} from "../src/lib/format.ts";

test("bytes: the acceptance cases", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(1024), "1.00 KB");
  assert.equal(formatBytes(1.5 * 1024 ** 4), "1.50 TB");
  assert.equal(formatBytes(-1536), "-1.50 KB");
});

test("bytes: identical to scanbench human_bytes", () => {
  assert.equal(formatBytes(1023), "1023 B");
  assert.equal(formatBytes(1536), "1.50 KB");
  assert.equal(formatBytes(1024 * 1024), "1.00 MB");
  assert.equal(formatBytes(222_298_112), "212 MB");
  assert.equal(formatBytes(51_765_093_335), "48.21 GB");
  assert.equal(formatBytes(1024 ** 5), "1.00 PB");
});

test("bytes: rounding promotes instead of printing 1024 of a unit", () => {
  assert.equal(formatBytes(Math.floor(1023.999 * 1024 * 1024)), "1.00 GB");
  assert.equal(formatBytes(1024 * 1024 - 1), "1.00 MB");
});

test("bytes: the largest unit absorbs anything bigger", () => {
  assert.equal(formatBytes(2048 * 1024 ** 5), "2048 PB");
});

test("bytes: fractions and non-numbers", () => {
  assert.equal(formatBytes(1.9), "1 B");
  assert.equal(formatBytes(Number.NaN), UNKNOWN);
  assert.equal(formatBytes(Number.POSITIVE_INFINITY), UNKNOWN);
  assert.equal(formatBytes(-0), "0 B");
});

test("counts group thousands independent of locale", () => {
  assert.equal(formatCount(0), "0");
  assert.equal(formatCount(999), "999");
  assert.equal(formatCount(1000), "1,000");
  assert.equal(formatCount(184_203), "184,203");
  assert.equal(formatCount(-1_234_567), "-1,234,567");
  assert.equal(formatCount(Number.NaN), UNKNOWN);
});

test("durations pick a readable unit", () => {
  assert.equal(formatDuration(0), "0 ms");
  assert.equal(formatDuration(850), "850 ms");
  assert.equal(formatDuration(4310), "4.31 s");
  assert.equal(formatDuration(125_000), "2 m 05 s");
  assert.equal(formatDuration(3_720_000), "1 h 02 m");
  assert.equal(formatDuration(-850), "-850 ms");
});

test("percent keeps one decimal", () => {
  assert.equal(formatPercent(0), "0.0%");
  assert.equal(formatPercent(0.4231), "42.3%");
  assert.equal(formatPercent(1), "100.0%");
});
