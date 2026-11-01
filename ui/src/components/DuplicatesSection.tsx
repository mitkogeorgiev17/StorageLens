/**
 * Duplicates (#40), a section of Cleanup (#112): resolve duplicates without
 * losing the only copy.
 *
 * A search runs only when asked (it hashes in the background and can take
 * minutes). Each group shows every copy with its size and date, plus a
 * suggested copy to keep — shown, never applied: nothing is ticked until a
 * click asks for it, and the receipt asks again. The last unticked copy of a
 * group cannot be ticked, and the backend refuses such a plan too. A group
 * disappears only once its copies are actually in the Trash.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import Receipt from "../components/Receipt";
import {
  api,
  onDupesProgress,
  type DeletionPlanDto,
  type DeletionResultDto,
  type DupesDto,
  type DupesProgressEvent,
  type DupGroupDto,
} from "../lib/api";
import {
  canSelect,
  keepOnly,
  prune,
  selectedIn,
  selectionTotal,
  suggestKeep,
  toggle,
  type DupSelection,
} from "../lib/duplicates";
import { formatBytes, formatCount, formatDate } from "../lib/format";
import { afterTrash } from "../lib/rescan";
import { useAppStore } from "../store";

/** Groups rendered per page (AGENTS.md §8: keep rendered rows bounded). */
const PAGE = 50;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function DuplicatesSection() {
  const scan = useAppStore((state) => state.scan);
  const scanId = scan.kind === "done" ? scan.summary.scanId : null;
  const [found, setFound] = useState<DupesDto | null>(null);
  const [searching, setSearching] = useState(false);
  const [progress, setProgress] = useState<DupesProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<DupSelection>(new Map());
  const [page, setPage] = useState(0);
  const [plan, setPlan] = useState<DeletionPlanDto | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setFound(null);
    setSelection(new Map());
    setPage(0);
  }, [scanId]);

  useEffect(() => {
    if (!searching) {
      return;
    }
    let stop: (() => void) | null = null;
    let live = true;
    void onDupesProgress((event) => {
      if (event.scanId === scanId) {
        setProgress(event);
      }
    }).then((unlisten) => {
      if (live) {
        stop = unlisten;
      } else {
        unlisten();
      }
    });
    return () => {
      live = false;
      stop?.();
    };
  }, [searching, scanId]);

  const load = useCallback((id: number, refresh: boolean) => {
    setError(null);
    return api
      .findDuplicates(id, refresh)
      .then((loaded) => {
        setFound(loaded);
        setSelection((current) => prune(current, loaded.groups));
      })
      .catch((cause: unknown) => setError(message(cause)));
  }, []);

  const search = (refresh: boolean) => {
    if (scanId === null) {
      return;
    }
    setSearching(true);
    setProgress(null);
    void load(scanId, refresh).finally(() => setSearching(false));
  };

  const groups = useMemo(() => found?.groups ?? [], [found]);
  const total = useMemo(
    () => selectionTotal(selection, groups),
    [selection, groups],
  );
  const reclaimable = groups.reduce((sum, group) => sum + group.reclaimable, 0);
  const pages = Math.max(1, Math.ceil(groups.length / PAGE));
  const shown = groups.slice(page * PAGE, page * PAGE + PAGE);

  if (scanId === null) {
    return null;
  }

  const review = () => {
    setBusy(true);
    setError(null);
    api
      .previewDuplicateRemoval(scanId, total.paths)
      .then(setPlan)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  const finished = (moved: DeletionResultDto) => {
    setPlan(null);
    afterTrash(moved);
    void load(scanId, false);
  };

  return (
    <section className="dupes-screen" aria-labelledby="dupes-title">
      <h2 id="dupes-title" className="cleanup-kind-title">
        Duplicate files
      </h2>
      <div className="apps-toolbar">
        <span className={searching ? "screen-status loading" : "screen-status"} aria-live="polite">
          {searching
            ? progress === null
              ? "Grouping files by size…"
              : `Comparing ${formatCount(progress.groupsDone)} of ${formatCount(
                  progress.groupsTotal,
                )} size groups · ${formatBytes(progress.bytesRead)} read`
            : found !== null
              ? `${formatCount(groups.length)} ${groups.length === 1 ? "group" : "groups"} · ${formatBytes(
                  reclaimable,
                )} reclaimable`
              : "Files of 1 MB or more with identical contents. Searching reads them in the background."}
        </span>
        {searching ? (
          <button
            type="button"
            className="button button-secondary"
            onClick={() => void api.cancelDuplicates()}
          >
            Stop
          </button>
        ) : (
          <button
            type="button"
            className="button"
            onClick={() => search(found !== null)}
          >
            {found === null ? "Find duplicates" : "Search again"}
          </button>
        )}
      </div>

      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}
      {found?.cancelled === true ? (
        <p className="map-notice">
          Stopped early. Only groups finished so far are listed.
        </p>
      ) : null}
      {found !== null && found.unreadable > 0 ? (
        <p className="empty-text">
          {formatCount(found.unreadable)}{" "}
          {found.unreadable === 1 ? "file was" : "files were"} skipped:
          unreadable, or changed since the scan.
        </p>
      ) : null}

      {found !== null && groups.length === 0 && !searching ? (
        <section className="empty" aria-labelledby="dupes-none">
          <h3 id="dupes-none">No duplicates</h3>
          <p className="empty-text">
            No two files of 1 MB or more have the same contents.
          </p>
        </section>
      ) : null}

      {shown.map((group) => (
        <GroupCard
          key={group.hash}
          group={group}
          selection={selection}
          onChange={setSelection}
        />
      ))}

      {pages > 1 ? (
        <nav className="dupes-pages" aria-label="Duplicate groups pages">
          <button
            type="button"
            className="button button-secondary"
            disabled={page === 0}
            onClick={() => setPage(page - 1)}
          >
            Previous
          </button>
          <span>
            Page {page + 1} of {pages}
          </span>
          <button
            type="button"
            className="button button-secondary"
            disabled={page + 1 >= pages}
            onClick={() => setPage(page + 1)}
          >
            Next
          </button>
        </nav>
      ) : null}

      {groups.length > 0 ? (
        <footer className="cleanup-footer">
          <span className="cleanup-total" aria-live="polite">
            {total.paths.length === 0
              ? "Nothing selected"
              : `${formatCount(total.paths.length)} ${
                  total.paths.length === 1 ? "copy" : "copies"
                } selected · ${formatBytes(total.bytes)}`}
          </span>
          <button
            type="button"
            className="button"
            disabled={total.paths.length === 0 || busy || searching}
            onClick={review}
          >
            Review and move to Trash…
          </button>
        </footer>
      ) : null}

      {plan !== null ? (
        <Receipt plan={plan} onCancel={() => setPlan(null)} onDone={finished} />
      ) : null}
    </section>
  );
}

function GroupCard({
  group,
  selection,
  onChange,
}: {
  readonly group: DupGroupDto;
  readonly selection: DupSelection;
  readonly onChange: (next: DupSelection) => void;
}) {
  const suggested = suggestKeep(group.files);
  const chosen = selectedIn(selection, group);
  const title = `dupes-${group.hash}`;
  return (
    <section className="dupes-group" aria-labelledby={title}>
      <header className="dupes-group-head">
        <h3 id={title} className="dupes-group-title">
          {formatCount(group.files.length)} copies ·{" "}
          {formatBytes(group.sizeLogical)} each
        </h3>
        <span className="dupes-reclaim">
          {formatBytes(group.reclaimable)} reclaimable
        </span>
      </header>
      {suggested !== null ? (
        <p className="dupes-suggest">
          Suggested: keep{" "}
          <span className="rule-path" title={suggested}>
            {suggested}
          </span>{" "}
          <button
            type="button"
            className="link-button"
            onClick={() => onChange(keepOnly(selection, group, suggested))}
          >
            Use suggestion
          </button>
        </p>
      ) : null}
      <ul className="dupes-files">
        {group.files.map((file) => {
          const ticked = chosen.has(file.path);
          const allowed = canSelect(selection, group, file.path);
          return (
            <li key={file.path} className="dupes-file" data-ticked={ticked}>
              <label
                title={allowed ? undefined : "At least one copy must stay"}
              >
                <input
                  type="checkbox"
                  checked={ticked}
                  disabled={!allowed}
                  onChange={() => onChange(toggle(selection, group, file.path))}
                />{" "}
                <span className="visually-hidden">Move to Trash: </span>
                <span className="rule-path">{file.path}</span>
              </label>
              <span className="dupes-meta">
                {formatDate(file.modified)} · {formatBytes(file.sizeAlloc)}
              </span>
              <button
                type="button"
                className="link-button"
                onClick={() => onChange(keepOnly(selection, group, file.path))}
              >
                Keep only this
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
