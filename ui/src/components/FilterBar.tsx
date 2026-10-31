/**
 * The Map's filter chips (#26): type, size, age, and location, composable —
 * a file must pass every chip that is set.
 *
 * - **Type** opens a checklist of file types (any number).
 * - **Size** and **Age** are single choices.
 * - **Location** appears when a non-folder view is limited to one folder
 *   (switching away from the Folder view inside a folder does that); its ×
 *   widens the view to the whole scan.
 *
 * A cleanliness filter follows the cleanup rules (#31): until rules give files
 * a verdict, every file would be "Unclassified".
 */

import { useEffect, useRef } from "react";

import type { Category } from "../lib/api";
import {
  AGE_OPTIONS,
  ALL_CATEGORIES,
  SIZE_OPTIONS,
  cleared,
  isFiltered,
  type AgeOption,
  type MapView,
  type SizeOption,
} from "../lib/mapView";
import { CATEGORY_LABELS } from "../lib/treemap";

function typeSummary(categories: readonly Category[]): string {
  if (categories.length === 0) {
    return "Any type";
  }
  const [first] = categories;
  return categories.length === 1 && first !== undefined
    ? CATEGORY_LABELS[first]
    : `${categories.length} types`;
}

export default function FilterBar({
  view,
  onChange,
}: {
  readonly view: MapView;
  readonly onChange: (view: MapView) => void;
}) {
  const typeMenu = useRef<HTMLDetailsElement | null>(null);

  // The type checklist closes on a click outside it, or Esc inside it.
  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      const menu = typeMenu.current;
      if (menu?.open && !menu.contains(event.target as Node)) {
        menu.open = false;
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, []);

  const toggle = (category: Category) => {
    const chosen = view.categories.includes(category)
      ? view.categories.filter((value) => value !== category)
      : [...view.categories, category];
    onChange({ ...view, categories: chosen });
  };

  const location = view.by === "folder" ? null : view.within;

  return (
    <div className="filter-bar" role="group" aria-label="Filters">
      <details
        ref={typeMenu}
        className="chip-menu"
        onKeyDown={(event) => {
          if (event.key === "Escape" && typeMenu.current?.open) {
            event.stopPropagation();
            typeMenu.current.open = false;
            typeMenu.current.querySelector("summary")?.focus();
          }
        }}
      >
        <summary className="chip" data-active={view.categories.length > 0}>
          <span className="visually-hidden">File type: </span>
          {typeSummary(view.categories)}
        </summary>
        <div className="chip-popover">
          <fieldset className="chip-checklist">
            <legend className="visually-hidden">File types to show</legend>
            {ALL_CATEGORIES.map((category) => (
              <label key={category} className="chip-option">
                <input
                  type="checkbox"
                  checked={view.categories.includes(category)}
                  onChange={() => toggle(category)}
                />
                {CATEGORY_LABELS[category]}
              </label>
            ))}
          </fieldset>
          {view.categories.length > 0 ? (
            <button
              type="button"
              className="chip-reset"
              onClick={() => onChange({ ...view, categories: [] })}
            >
              Any type
            </button>
          ) : null}
        </div>
      </details>

      <select
        className="chip"
        aria-label="Minimum size"
        data-active={view.minSize !== null}
        value={view.minSize ?? ""}
        onChange={(event) =>
          onChange({
            ...view,
            minSize: event.target.value === "" ? null : (event.target.value as SizeOption),
          })
        }
      >
        <option value="">Any size</option>
        {Object.entries(SIZE_OPTIONS).map(([key, option]) => (
          <option key={key} value={key}>
            {option.label}
          </option>
        ))}
      </select>

      <select
        className="chip"
        aria-label="Age"
        data-active={view.olderThan !== null}
        value={view.olderThan ?? ""}
        onChange={(event) =>
          onChange({
            ...view,
            olderThan: event.target.value === "" ? null : (event.target.value as AgeOption),
          })
        }
      >
        <option value="">Any age</option>
        {Object.entries(AGE_OPTIONS).map(([key, option]) => (
          <option key={key} value={key}>
            {option.label}
          </option>
        ))}
      </select>

      {location === null ? null : (
        <button
          type="button"
          className="chip"
          data-active
          aria-label={`Only in ${location.name}. Remove to show the whole scan.`}
          onClick={() => onChange({ ...view, within: null })}
        >
          In {location.name}
          <span aria-hidden="true" className="chip-remove">
            ×
          </span>
        </button>
      )}

      {isFiltered(view) ? (
        <button type="button" className="chip-clear" onClick={() => onChange(cleared(view))}>
          Clear filters
        </button>
      ) : null}
    </div>
  );
}
