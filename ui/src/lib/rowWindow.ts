/**
 * Which pages of a positional listing to hold (AGENTS.md §8: no component
 * holds more than ~200 rows). The table asks for the pages covering the rows
 * on screen; everything else is evicted. Pure, so it is tested under Node.
 */

export const PAGE_SIZE = 100;
/** Pages held at once: the screen never spans more than two. */
export const MAX_PAGES = 2;

/** Pages covering rows `start ..= stop` (inclusive), clamped to `total`. */
export function pagesFor(start: number, stop: number, total: number): number[] {
  if (total <= 0 || stop < start) {
    return [];
  }
  const first = Math.max(0, Math.floor(start / PAGE_SIZE));
  const last = Math.min(Math.floor((total - 1) / PAGE_SIZE), Math.floor(stop / PAGE_SIZE));
  const pages: number[] = [];
  for (let page = first; page <= last && pages.length < MAX_PAGES; page += 1) {
    pages.push(page);
  }
  return pages;
}

/** Drop every page not in `keep`. Returns whether anything was evicted. */
export function evict<T>(pages: Map<number, T>, keep: readonly number[]): boolean {
  let evicted = false;
  for (const page of [...pages.keys()]) {
    if (!keep.includes(page)) {
      pages.delete(page);
      evicted = true;
    }
  }
  return evicted;
}

/** Row `index` if its page is held. */
export function rowAt<T>(pages: ReadonlyMap<number, readonly T[]>, index: number): T | undefined {
  return pages.get(Math.floor(index / PAGE_SIZE))?.[index % PAGE_SIZE];
}
