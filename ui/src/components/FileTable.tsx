/**
 * A virtualized file table (react-window): only the rows on screen are in the
 * DOM, and only the pages the parent holds have data. A row whose page has not
 * arrived yet renders as a placeholder, so the scrollbar can be dragged
 * anywhere in a million rows.
 *
 * Keyboard: ↑/↓, Page Up/Down, Home/End move the selection; Enter (or
 * double-click) reveals; Space previews — reported to the parent.
 *
 * Height limit: browsers cap an element near 33.5M px, so at 30 px a row the
 * scrollbar spans about 1.1M rows. Bigger listings still scroll, but the last
 * rows need the size filter.
 */

import { type CSSProperties, type KeyboardEvent, type RefObject } from "react";
import { List, type ListImperativeAPI, type RowComponentProps } from "react-window";

import type { FileRowDto, FileSort } from "../lib/api";
import { formatBytes, formatDate } from "../lib/format";
import { CATEGORY_LABELS } from "../lib/treemap";

export const ROW_HEIGHT = 30;
/** Rows a Page Up/Down moves. */
const PAGE_STEP = 15;

export interface FileTableProps {
  readonly total: number;
  readonly rowAt: (index: number) => FileRowDto | undefined;
  /** Bumped by the parent when rows arrive, to redraw placeholders. */
  readonly version: number;
  readonly selectedIndex: number | null;
  readonly onSelect: (index: number) => void;
  readonly onReveal: (index: number) => void;
  readonly onPreview: (index: number) => void;
  readonly onRangeChange: (start: number, stop: number) => void;
  readonly sort: FileSort;
  readonly reversed: boolean;
  readonly onSort: (sort: FileSort) => void;
  readonly listRef: RefObject<ListImperativeAPI | null>;
  readonly label: string;
}

interface RowProps {
  readonly rowAt: (index: number) => FileRowDto | undefined;
  readonly version: number;
  readonly selectedIndex: number | null;
  readonly onSelect: (index: number) => void;
  readonly onReveal: (index: number) => void;
}

/** The folder part of a path, for the last column. */
function folderOf(path: string, name: string): string {
  return path.endsWith(name) ? path.slice(0, path.length - name.length).replace(/[\\/]$/, "") : path;
}

function Row({
  index,
  style,
  rowAt,
  selectedIndex,
  onSelect,
  onReveal,
}: RowComponentProps<RowProps>) {
  const row = rowAt(index);
  const selected = index === selectedIndex;
  const common = {
    role: "row",
    "aria-rowindex": index + 2,
    "aria-selected": selected,
    className: row === undefined ? "file-row file-row-pending" : "file-row",
    style: style as CSSProperties,
    onClick: () => onSelect(index),
    onDoubleClick: () => onReveal(index),
  } as const;
  if (row === undefined) {
    return (
      <div {...common}>
        <span role="gridcell" className="file-cell-name">
          <span className="placeholder" />
        </span>
      </div>
    );
  }
  return (
    <div {...common}>
      <span role="gridcell" className="file-cell-name" title={row.name}>
        {row.name}
      </span>
      <span role="gridcell" className="file-cell-size">
        {formatBytes(row.sizeAlloc)}
      </span>
      <span role="gridcell" className="file-cell-type">
        {CATEGORY_LABELS[row.category]}
      </span>
      <span role="gridcell" className="file-cell-date">
        {formatDate(row.modified)}
      </span>
      <span role="gridcell" className="file-cell-path" title={row.path}>
        {folderOf(row.path, row.name)}
      </span>
    </div>
  );
}

const COLUMNS: readonly { readonly key: FileSort | null; readonly label: string; readonly className: string }[] = [
  { key: null, label: "Name", className: "file-cell-name" },
  { key: "size", label: "Size", className: "file-cell-size" },
  { key: "type", label: "Type", className: "file-cell-type" },
  { key: "modified", label: "Modified", className: "file-cell-date" },
  { key: null, label: "Folder", className: "file-cell-path" },
];

/** What a sort key's natural direction means, for the header's aria-sort. */
const NATURAL: Readonly<Record<FileSort, "ascending" | "descending">> = {
  size: "descending",
  modified: "ascending",
  type: "ascending",
};

function ariaSort(key: FileSort, sort: FileSort, reversed: boolean) {
  if (key !== sort) {
    return undefined;
  }
  const natural = NATURAL[key];
  return reversed ? (natural === "ascending" ? "descending" : "ascending") : natural;
}

export default function FileTable({
  total,
  rowAt,
  version,
  selectedIndex,
  onSelect,
  onReveal,
  onPreview,
  onRangeChange,
  sort,
  reversed,
  onSort,
  listRef,
  label,
}: FileTableProps) {
  const move = (to: number) => {
    if (total === 0) {
      return;
    }
    const index = Math.min(total - 1, Math.max(0, to));
    onSelect(index);
    listRef.current?.scrollToRow({ index, align: "auto" });
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = selectedIndex ?? -1;
    const steps: Readonly<Record<string, number>> = {
      ArrowDown: current + 1,
      ArrowUp: current - 1,
      PageDown: current + PAGE_STEP,
      PageUp: current - PAGE_STEP,
      Home: 0,
      End: total - 1,
    };
    const target = steps[event.key];
    if (target !== undefined) {
      event.preventDefault();
      move(target);
      return;
    }
    if (selectedIndex === null) {
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      onReveal(selectedIndex);
    } else if (event.key === " ") {
      event.preventDefault();
      onPreview(selectedIndex);
    }
  };

  return (
    <div
      className="file-table"
      role="grid"
      aria-label={label}
      aria-rowcount={total + 1}
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      <div className="file-row file-header" role="row" aria-rowindex={1}>
        {COLUMNS.map((column) =>
          column.key === null ? (
            <span key={column.label} role="columnheader" className={column.className}>
              {column.label}
            </span>
          ) : (
            <span
              key={column.label}
              role="columnheader"
              className={column.className}
              aria-sort={ariaSort(column.key, sort, reversed)}
            >
              <button
                type="button"
                className="sort-button"
                // Header buttons are reached with Tab; arrows stay with the rows.
                onKeyDown={(event) => event.stopPropagation()}
                onClick={() => onSort(column.key as FileSort)}
              >
                {column.label}
                {column.key === sort ? (
                  <span aria-hidden="true">{reversed ? " ↑" : " ↓"}</span>
                ) : null}
              </button>
            </span>
          ),
        )}
      </div>
      <List
        className="file-list"
        listRef={listRef}
        rowCount={total}
        rowHeight={ROW_HEIGHT}
        rowComponent={Row}
        rowProps={{ rowAt, version, selectedIndex, onSelect, onReveal }}
        overscanCount={10}
        onRowsRendered={(visible) => onRangeChange(visible.startIndex, visible.stopIndex)}
      />
    </div>
  );
}
