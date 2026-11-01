/**
 * The Large Files screen (#23): every file of the scan, sortable by size, age,
 * or type, filtered by a minimum size.
 *
 * The backend keeps the order in memory and serves rows by position
 * (`get_files`); this screen holds at most two 100-row pages (`rowWindow.ts`)
 * — the ones on screen — and drops the rest. Changing the sort or filter asks
 * the backend for a new order and starts again at the top; it never refetches
 * a whole set.
 *
 * Keyed by scan id, so a rescan starts clean. Every listener is an element
 * prop; the fetch timer and late responses are dropped on unmount.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useListRef } from "react-window";

import FileTable from "../components/FileTable";
import { api, type FileRowDto, type FileSort } from "../lib/api";
import { formatBytes, formatCount } from "../lib/format";
import { PAGE_SIZE, evict, pagesFor, rowAt } from "../lib/rowWindow";
import { hasScan, useAppStore, type ScanSummary } from "../store";

const MIB = 1024 * 1024;
const MIN_SIZES = [0, MIB, 10 * MIB, 100 * MIB, 1024 * MIB] as const;
/** Wait for scrolling to settle before fetching, so a fast drag fetches once. */
const FETCH_DELAY_MS = 40;

const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function LargeFilesScreen() {
  const scan = useAppStore((state) => state.scan);
  if (!hasScan(scan)) {
    return (
      <section className="empty" aria-labelledby="files-empty">
        <h2 id="files-empty">
          {scan.kind === "scanning"
            ? "Files are listed when the scan finishes."
            : "Large files are listed after the first scan. Start one from the Overview."}
        </h2>
        <p className="empty-text">Every file, biggest first, filterable by type and age.</p>
      </section>
    );
  }
  return <LargeFilesView key={scan.summary.scanId} summary={scan.summary} />;
}

function LargeFilesView({ summary }: { readonly summary: ScanSummary }) {
  const { scanId } = summary;
  const [sort, setSort] = useState<FileSort>("size");
  const [reversed, setReversed] = useState(false);
  const [minSize, setMinSize] = useState<number>(MIN_SIZES[1]);
  const [total, setTotal] = useState<number | null>(null);
  const [version, setVersion] = useState(0);
  const [selected, setSelected] = useState<{ index: number; row: FileRowDto | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const listRef = useListRef(null);
  const pages = useRef(new Map<number, readonly FileRowDto[]>());
  const inflight = useRef(new Set<number>());
  /** Bumped when the order changes; responses for an older order are dropped. */
  const generation = useRef(0);
  const wanted = useRef<number[]>([0]);
  const timer = useRef<number | null>(null);
  const mounted = useRef(true);

  const fetchPage = useCallback(
    (page: number) => {
      if (pages.current.has(page) || inflight.current.has(page)) {
        return;
      }
      const ticket = generation.current;
      inflight.current.add(page);
      api
        .getFiles(scanId, sort, reversed, minSize, page * PAGE_SIZE, PAGE_SIZE)
        .then((range) => {
          if (!mounted.current || ticket !== generation.current) {
            return;
          }
          inflight.current.delete(page);
          setTotal(range.total);
          // Only keep it if it is still on screen.
          if (wanted.current.includes(page)) {
            pages.current.set(page, range.items);
            setVersion((value) => value + 1);
          }
        })
        .catch((cause: unknown) => {
          if (mounted.current && ticket === generation.current) {
            inflight.current.delete(page);
            setError(message(cause));
          }
        });
    },
    [scanId, sort, reversed, minSize],
  );

  // A new order: forget every row and start at the top.
  useEffect(() => {
    generation.current += 1;
    pages.current.clear();
    inflight.current.clear();
    wanted.current = [0];
    setTotal(null);
    setSelected(null);
    setError(null);
    // Not scrollToRow: it throws on an empty list (the first render has 0 rows).
    listRef.current?.element?.scrollTo({ top: 0 });
    fetchPage(0);
  }, [fetchPage, listRef]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (timer.current !== null) {
        window.clearTimeout(timer.current);
      }
    };
  }, []);

  const onRangeChange = (start: number, stop: number) => {
    if (total === null) {
      return;
    }
    const needed = pagesFor(start, stop, total);
    wanted.current = needed;
    if (evict(pages.current, needed)) {
      setVersion((value) => value + 1);
    }
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
    }
    timer.current = window.setTimeout(() => {
      timer.current = null;
      for (const page of wanted.current) {
        fetchPage(page);
      }
    }, FETCH_DELAY_MS);
  };

  const lookup = useCallback((index: number) => rowAt(pages.current, index), []);

  const select = (index: number) => {
    setSelected({ index, row: rowAt(pages.current, index) ?? null });
  };

  // A selection made on a placeholder fills in when its page arrives.
  const selectedRow =
    selected === null ? null : (selected.row ?? rowAt(pages.current, selected.index) ?? null);

  const run = (action: "reveal" | "preview", row: FileRowDto | null | undefined) => {
    if (row === null || row === undefined) {
      return;
    }
    setNotice(null);
    const call = action === "reveal" ? api.revealEntry : api.previewEntry;
    call(scanId, row.id).catch((cause: unknown) => {
      if (mounted.current) {
        setNotice(message(cause));
      }
    });
  };

  const onSort = (key: FileSort) => {
    if (key === sort) {
      setReversed((value) => !value);
    } else {
      setSort(key);
      setReversed(false);
    }
  };

  const fileManager = IS_MAC ? "Finder" : "Explorer";

  return (
    <div className="files-screen">
      <div className="files-toolbar">
        <label className="files-filter">
          At least
          <select
            className="select"
            value={minSize}
            onChange={(event) => setMinSize(Number(event.target.value))}
          >
            {MIN_SIZES.map((size) => (
              <option key={size} value={size}>
                {size === 0 ? "any size" : formatBytes(size)}
              </option>
            ))}
          </select>
        </label>
        <span className="files-count" aria-live="polite">
          {total === null ? <span className="loading">Loading…</span> : `${formatCount(total)} files`}
        </span>
        <span className="files-actions">
          <button
            type="button"
            className="button button-secondary"
            disabled={selectedRow === null}
            onClick={() => run("reveal", selectedRow)}
          >
            Show in {fileManager}
          </button>
          <button
            type="button"
            className="button button-secondary"
            disabled={selectedRow === null}
            onClick={() => run("preview", selectedRow)}
          >
            {IS_MAC ? "Quick Look" : "Preview"}
          </button>
        </span>
      </div>

      {error !== null ? (
        <p className="picker-error" role="alert">
          {error}
        </p>
      ) : null}

      {total === 0 ? (
        <section className="empty" aria-live="polite">
          <h2>No files {minSize > 0 ? `of ${formatBytes(minSize)} or more` : ""}</h2>
          <p className="empty-text">Lower the size filter to see smaller files.</p>
        </section>
      ) : (
        <FileTable
          total={total ?? 0}
          rowAt={lookup}
          version={version}
          selectedIndex={selected?.index ?? null}
          onSelect={select}
          onReveal={(index) => {
            select(index);
            run("reveal", rowAt(pages.current, index));
          }}
          onPreview={(index) => run("preview", rowAt(pages.current, index))}
          onRangeChange={onRangeChange}
          sort={sort}
          reversed={reversed}
          onSort={onSort}
          listRef={listRef}
          label={`Files${minSize > 0 ? ` of ${formatBytes(minSize)} or more` : ""}, sorted by ${sort}`}
        />
      )}

      <p className="map-hints">
        {notice !== null ? (
          <span className="map-notice" role="alert">
            {notice}
          </span>
        ) : (
          <>
            Sizes are space on disk · ↑↓ to move · Enter or double-click to show in {fileManager}{" "}
            · Space to {IS_MAC ? "Quick Look" : "preview"}
          </>
        )}
      </p>
    </div>
  );
}
