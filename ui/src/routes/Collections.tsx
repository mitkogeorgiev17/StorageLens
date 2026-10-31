/**
 * Smart collections (#28): saved Map views, re-run against the current scan.
 *
 * Each collection shows what it totals in the scan that is current now. A new
 * scan re-evaluates every collection, and a total is only shown when it was
 * computed for the current scan id, so nothing stale is ever on screen. A
 * collection whose stored query cannot be read is listed with the reason and
 * can still be renamed or deleted.
 */

import { useEffect, useRef, useState, type FormEvent } from "react";

import { api, type CollectionDto, type CollectionResultDto } from "../lib/api";
import { openCollection, refreshCollections } from "../lib/collectionActions";
import { currentResult, describeQuery } from "../lib/collections";
import { formatBytes, formatCount } from "../lib/format";
import { useAppStore } from "../store";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function CollectionsScreen() {
  const collections = useAppStore((state) => state.collections);
  const scan = useAppStore((state) => state.scan);
  const scanId = scan.kind === "done" ? scan.summary.scanId : null;
  const [results, setResults] = useState<ReadonlyMap<number, CollectionResultDto>>(new Map());
  const [notice, setNotice] = useState<string | null>(null);
  const ticket = useRef(0);

  useEffect(() => {
    if (collections === null) {
      refreshCollections().catch((error: unknown) => setNotice(message(error)));
    }
  }, [collections]);

  // Re-run every readable collection against the current scan, one by one.
  useEffect(() => {
    const mine = ++ticket.current;
    if (scanId === null || collections === null) {
      return;
    }
    void (async () => {
      for (const collection of collections) {
        if (collection.query === null) {
          continue;
        }
        try {
          const result = await api.evaluateCollection(scanId, collection.id);
          if (mine !== ticket.current) {
            return;
          }
          setResults((known) => new Map(known).set(collection.id, result));
        } catch (error: unknown) {
          if (mine === ticket.current) {
            setNotice(message(error));
          }
        }
      }
    })();
  }, [scanId, collections]);

  if (collections === null) {
    return <p className="collections-status">Loading collections…</p>;
  }

  if (collections.length === 0) {
    return (
      <section className="empty" aria-labelledby="collections-empty">
        <h2 id="collections-empty">No collections yet</h2>
        <p className="empty-text">
          On the Map, pick a grouping and filters (say, Video, 500 MB or more, older than a year),
          then choose “Save view”. It re-runs on every scan.
        </p>
      </section>
    );
  }

  return (
    <div className="collections-screen">
      <p className="collections-status" aria-live="polite">
        {notice ??
          (scan.kind === "scanning"
            ? "A scan is running. Totals update when it finishes."
            : scanId === null
              ? "Totals appear after a scan."
              : "Totals are for the current scan.")}
      </p>
      <ul className="collection-list">
        {collections.map((collection) => (
          <CollectionItem
            key={collection.id}
            collection={collection}
            result={currentResult(results, collection.id, scanId)}
            scanId={scanId}
            onNotice={setNotice}
          />
        ))}
      </ul>
    </div>
  );
}

function CollectionItem({
  collection,
  result,
  scanId,
  onNotice,
}: {
  readonly collection: CollectionDto;
  readonly result: CollectionResultDto | null;
  readonly scanId: number | null;
  readonly onNotice: (text: string | null) => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [name, setName] = useState(collection.name);

  const rename = (event: FormEvent) => {
    event.preventDefault();
    api
      .renameCollection(collection.id, name)
      .then(() => refreshCollections())
      .then(() => {
        setRenaming(false);
        onNotice(null);
      })
      .catch((error: unknown) => onNotice(message(error)));
  };

  const remove = () => {
    api
      .deleteCollection(collection.id)
      .then(() => refreshCollections())
      .then(() => onNotice(`Deleted “${collection.name}”.`))
      .catch((error: unknown) => onNotice(message(error)));
  };

  const open = () => {
    if (scanId === null) {
      return;
    }
    openCollection(collection, scanId).catch((error: unknown) => onNotice(message(error)));
  };

  const problem = collection.problem ?? result?.problem ?? null;
  const total =
    collection.query === null
      ? null
      : scanId === null
        ? "—"
        : result === null
          ? "Updating…"
          : result.problem !== null
            ? null
            : `${formatBytes(result.bytes)} in ${formatCount(result.files)} ${
                result.files === 1 ? "file" : "files"
              }`;

  return (
    <li className="collection">
      <div className="collection-main">
        {renaming ? (
          <form className="collection-rename" onSubmit={rename}>
            <input
              className="input"
              aria-label="Collection name"
              value={name}
              autoFocus
              maxLength={120}
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  setRenaming(false);
                  setName(collection.name);
                }
              }}
            />
            <button type="submit" className="button">
              Save
            </button>
          </form>
        ) : (
          <h2 className="collection-name">{collection.name}</h2>
        )}
        <p className="collection-query">
          {collection.query === null ? "Cannot be opened by this version" : describeQuery(collection.query)}
        </p>
        {problem !== null ? (
          <p className="collection-problem">{problem}</p>
        ) : total !== null ? (
          <p className="collection-total">{total}</p>
        ) : null}
      </div>
      <div className="collection-actions">
        {confirming ? (
          <>
            <span className="collection-confirm">Delete “{collection.name}”?</span>
            <button type="button" className="button button-danger" onClick={remove}>
              Delete
            </button>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => setConfirming(false)}
            >
              Cancel
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              className="button"
              disabled={scanId === null || problem !== null}
              onClick={open}
            >
              Open in Map
            </button>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => {
                setName(collection.name);
                setRenaming(true);
              }}
            >
              Rename
            </button>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => setConfirming(true)}
            >
              Delete
            </button>
          </>
        )}
      </div>
    </li>
  );
}
