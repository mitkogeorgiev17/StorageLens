/**
 * "Save view" on the Map (#28): saves the current grouping, chart, filters,
 * and folder as a smart collection. The folder is saved by path, so the
 * collection works against later scans too.
 */

import { useEffect, useRef, useState, type FormEvent } from "react";

import { saveCollection } from "../lib/collectionActions";
import { describeQuery, viewToQuery } from "../lib/collections";
import type { MapView } from "../lib/mapView";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function SaveCollection({
  view,
  folderPath,
  onSaved,
}: {
  readonly view: MapView;
  /** The folder the view is limited to (or open at), as an absolute path. */
  readonly folderPath: () => Promise<string | null>;
  readonly onSaved: (text: string) => void;
}) {
  const menu = useRef<HTMLDetailsElement | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (menu.current?.open && !menu.current.contains(event.target as Node)) {
        menu.current.open = false;
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, []);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setError(null);
    folderPath()
      .then((path) => {
        const chosen = name.trim() === "" ? describeQuery(viewToQuery(view, path)) : name;
        return saveCollection(chosen, view, path);
      })
      .then((saved) => {
        setName("");
        if (menu.current !== null) {
          menu.current.open = false;
        }
        onSaved(`Saved as “${saved.name}”. Find it under Collections.`);
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setSaving(false));
  };

  return (
    <details
      ref={menu}
      className="chip-menu save-collection"
      onKeyDown={(event) => {
        if (event.key === "Escape" && menu.current?.open) {
          event.stopPropagation();
          menu.current.open = false;
          menu.current.querySelector("summary")?.focus();
        }
      }}
    >
      <summary className="chip">Save view…</summary>
      <form className="chip-popover save-form" onSubmit={submit}>
        <label className="save-label">
          Name
          <input
            className="input"
            value={name}
            maxLength={120}
            placeholder={describeQuery(viewToQuery(view, null))}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
        <p className="save-hint">Saved views re-run on every scan.</p>
        {error !== null ? (
          <p className="map-notice" role="alert">
            {error}
          </p>
        ) : null}
        <button type="submit" className="button" disabled={saving}>
          {saving ? "Saving…" : "Save as collection"}
        </button>
      </form>
    </details>
  );
}
