// The Large Files row window. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { MAX_PAGES, PAGE_SIZE, evict, pagesFor, rowAt } from "../src/lib/rowWindow.ts";

test("the pages for a visible range, clamped to the listing", () => {
  assert.deepEqual(pagesFor(0, 29, 1_000_000), [0]);
  assert.deepEqual(pagesFor(90, 119, 1_000_000), [0, 1]);
  assert.deepEqual(pagesFor(600_000, 600_030, 1_000_000), [6000]);
  assert.deepEqual(pagesFor(0, 29, 10), [0], "past the end is clamped");
  assert.deepEqual(pagesFor(0, 29, 0), []);
  assert.deepEqual(pagesFor(5, 4, 100), [], "an empty range needs nothing");
  // A range wider than the screen still never asks for more than two pages.
  assert.equal(pagesFor(0, 10 * PAGE_SIZE, 1_000_000).length, MAX_PAGES);
});

test("scrolling through a million rows never holds more than 200", () => {
  const pages = new Map<number, number[]>();
  let peak = 0;
  for (let top = 0; top < 1_000_000; top += 37) {
    const needed = pagesFor(top, top + 30, 1_000_000);
    evict(pages, needed);
    for (const page of needed) {
      if (!pages.has(page)) {
        pages.set(
          page,
          Array.from({ length: PAGE_SIZE }, (_, offset) => page * PAGE_SIZE + offset),
        );
      }
    }
    const held = [...pages.values()].reduce((sum, rows) => sum + rows.length, 0);
    peak = Math.max(peak, held);
    assert.equal(rowAt(pages, top), top, "the top row is loaded and in place");
  }
  assert.ok(peak <= MAX_PAGES * PAGE_SIZE, `held ${peak} rows`);
});

test("rows outside the held pages are absent, not wrong", () => {
  const pages = new Map([[3, ["a", "b"]]]);
  assert.equal(rowAt(pages, 300), "a");
  assert.equal(rowAt(pages, 301), "b");
  assert.equal(rowAt(pages, 302), undefined);
  assert.equal(rowAt(pages, 0), undefined);
  assert.equal(evict(pages, [3]), false);
  assert.equal(evict(pages, []), true);
  assert.equal(pages.size, 0);
});
