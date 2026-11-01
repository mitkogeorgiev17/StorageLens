/**
 * Filename search (#24) in the header: type to search the current scan's
 * files and folders by name, biggest first.
 *
 * A combobox with a results listbox (WAI-ARIA combobox pattern): focus stays
 * in the input, ↑/↓ move the active result.
 *
 * | Action             | Mouse            | Keyboard               |
 * | ------------------ | ---------------- | ---------------------- |
 * | Show in the Map    | click a result   | Enter                  |
 * | Show in Finder/Explorer | row button  | ⌘/Ctrl+Enter           |
 * | Quick Look/preview | row button       | ⌘/Ctrl+Y               |
 * | Close              | click outside    | Esc (again: clear)     |
 *
 * Results come 50 per page with "Show more", up to 200 (AGENTS.md §8). Typing
 * is debounced, and a response for an older query is dropped.
 */

import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";

import { api, type FileRowDto, type SearchCursor } from "../lib/api";
import { formatBytes, formatCount } from "../lib/format";
import { CATEGORY_LABELS } from "../lib/treemap";
import { useAppStore } from "../store";

const DEBOUNCE_MS = 150;
const MAX_RESULTS = 200;
const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface Results {
  readonly query: string;
  readonly items: readonly FileRowDto[];
  readonly next: SearchCursor | null;
}

export default function SearchBar() {
  const scan = useAppStore((state) => state.scan);
  const showInMap = useAppStore((state) => state.showInMap);
  const scanId = scan.kind === "done" ? scan.summary.scanId : null;

  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Results | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  // Collapsed to an icon until clicked; collapses again once empty and left.
  const [expanded, setExpanded] = useState(false);

  const rootRef = useRef<HTMLDivElement | null>(null);
  const ticket = useRef(0);
  const listId = useId();

  // A new scan (or none): start over.
  useEffect(() => {
    ticket.current += 1;
    setQuery("");
    setResults(null);
    setOpen(false);
  }, [scanId]);

  // Debounced search for the typed query.
  useEffect(() => {
    const trimmed = query.trim();
    if (scanId === null || trimmed === "") {
      ticket.current += 1;
      setResults(null);
      setLoading(false);
      return;
    }
    const mine = ++ticket.current;
    setLoading(true);
    const timer = window.setTimeout(() => {
      api
        .search(scanId, trimmed)
        .then((page) => {
          if (mine === ticket.current) {
            setResults({ query: trimmed, items: page.items, next: page.nextCursor });
            setActive(0);
            setError(null);
            setLoading(false);
          }
        })
        .catch((cause: unknown) => {
          if (mine === ticket.current) {
            setError(message(cause));
            setLoading(false);
          }
        });
    }, DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [query, scanId]);

  // Close when focus or a click leaves the search; collapse it if empty.
  useEffect(() => {
    if (!open && !expanded) {
      return;
    }
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) {
        setOpen(false);
        if (query === "") {
          setExpanded(false);
        }
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open, expanded, query]);

  const more = () => {
    if (scanId === null || results === null || results.next === null) {
      return;
    }
    const mine = ++ticket.current;
    const { query: current, next, items } = results;
    api
      .search(scanId, current, next)
      .then((page) => {
        if (mine === ticket.current) {
          const combined = [...items, ...page.items].slice(0, MAX_RESULTS);
          setResults({
            query: current,
            items: combined,
            next: combined.length >= MAX_RESULTS ? null : page.nextCursor,
          });
        }
      })
      .catch((cause: unknown) => mine === ticket.current && setError(message(cause)));
  };

  const run = (action: "reveal" | "preview", row: FileRowDto | undefined) => {
    if (scanId === null || row === undefined) {
      return;
    }
    setNotice(null);
    const call = action === "reveal" ? api.revealEntry : api.previewEntry;
    call(scanId, row.id).catch((cause: unknown) => setNotice(message(cause)));
  };

  const choose = (row: FileRowDto | undefined) => {
    if (row === undefined) {
      return;
    }
    setOpen(false);
    showInMap(row.id, row.kind === "Dir");
  };

  const items = results?.items ?? [];
  const activeRow = items[active];
  const capped = items.length >= MAX_RESULTS;

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    const modifier = IS_MAC ? event.metaKey : event.ctrlKey;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setOpen(true);
      if (items.length > 0) {
        const step = event.key === "ArrowDown" ? 1 : -1;
        setActive((value) => (value + step + items.length) % items.length);
      }
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (modifier) {
        run("reveal", activeRow);
      } else {
        choose(activeRow);
      }
    } else if (modifier && event.key.toLowerCase() === "y") {
      event.preventDefault();
      run("preview", activeRow);
    } else if (event.key === "Escape") {
      if (open && query !== "") {
        setOpen(false);
      } else if (query !== "") {
        setQuery("");
      } else {
        setExpanded(false);
      }
    }
  };

  const disabled = scanId === null;
  const showPanel = open && query.trim() !== "";
  const fileManager = IS_MAC ? "Finder" : "Explorer";

  if (!expanded) {
    return (
      <button
        type="button"
        className="search-toggle"
        aria-label={disabled ? "Search after a scan" : "Search by name"}
        title={disabled ? "Search after a scan" : "Search by name"}
        disabled={disabled}
        onClick={() => setExpanded(true)}
      >
        <svg viewBox="0 0 24 24" width={18} height={18} fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" aria-hidden="true">
          <circle cx="11" cy="11" r="7" />
          <path d="M20 20l-4-4" />
        </svg>
      </button>
    );
  }

  return (
    <div className="search" ref={rootRef}>
      <input
        autoFocus
        className="input search-input"
        type="search"
        role="combobox"
        aria-label="Search files and folders by name"
        aria-expanded={showPanel}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={showPanel && activeRow !== undefined ? `${listId}-${active}` : undefined}
        placeholder={disabled ? "Search after a scan" : "Search by name"}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={onKeyDown}
      />
      {showPanel ? (
        <div className="search-panel">
          <ul id={listId} role="listbox" aria-label="Search results" className="search-results">
            {items.map((row, index) => (
              <li
                key={row.id}
                id={`${listId}-${index}`}
                role="option"
                aria-selected={index === active}
                className="search-result"
                onPointerMove={() => setActive(index)}
                onClick={() => choose(row)}
              >
                <span className="search-result-name">
                  {row.name}
                  <span className="search-result-meta">
                    {" · "}
                    {row.kind === "Dir" ? "Folder" : CATEGORY_LABELS[row.category]} ·{" "}
                    {formatBytes(row.sizeAlloc)}
                  </span>
                </span>
                <span className="search-result-path">{row.path}</span>
                <span className="search-result-actions">
                  <button
                    type="button"
                    className="search-action"
                    tabIndex={-1}
                    onClick={(event) => {
                      event.stopPropagation();
                      run("reveal", row);
                    }}
                  >
                    Show in {fileManager}
                  </button>
                  <button
                    type="button"
                    className="search-action"
                    tabIndex={-1}
                    onClick={(event) => {
                      event.stopPropagation();
                      run("preview", row);
                    }}
                  >
                    {IS_MAC ? "Quick Look" : "Preview"}
                  </button>
                </span>
              </li>
            ))}
          </ul>
          <div className="search-footer" aria-live="polite">
            {error !== null ? (
              <span className="map-notice">{error}</span>
            ) : notice !== null ? (
              <span className="map-notice">{notice}</span>
            ) : loading ? (
              "Searching…"
            ) : items.length === 0 ? (
              `No names match “${query.trim()}”.`
            ) : results?.next !== null ? (
              <button type="button" className="search-more" onClick={more}>
                Show more
              </button>
            ) : capped ? (
              "Showing the 200 biggest matches. Type more to narrow the search."
            ) : (
              `${formatCount(items.length)} ${items.length === 1 ? "match" : "matches"}`
            )}
            <span className="search-hint">
              Enter: show in Map · {IS_MAC ? "⌘" : "Ctrl+"}Enter: {fileManager} ·{" "}
              {IS_MAC ? "⌘Y: Quick Look" : "Ctrl+Y: preview"}
            </span>
          </div>
        </div>
      ) : null}
    </div>
  );
}
