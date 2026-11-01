/**
 * Docker (#168), a section of Cleanup: what fills Docker's disk image, and
 * removing what is unused.
 *
 * The listing comes from the local Docker daemon through the docker CLI.
 * Items in use cannot be ticked; unused items older than a month start
 * ticked, except volumes. Docker has no Trash, so removal asks once more and
 * says it is permanent. The backend checks each item against a fresh listing
 * before removing it. Hidden when Docker is not installed.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import {
  api,
  type DockerDto,
  type DockerItemDto,
  type DockerRemovalDto,
} from "../lib/api";
import {
  DOCKER_KINDS,
  dockerKey,
  dockerSelected,
  initialDockerSelection,
  toggleDocker,
  type DockerSelection,
} from "../lib/docker";
import { formatBytes, formatCount, formatDate } from "../lib/format";
import { requestRescan } from "../lib/rescan";
import InfoTip from "./InfoTip";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function DockerSection() {
  const [docker, setDocker] = useState<DockerDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [selection, setSelection] = useState<DockerSelection>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<DockerRemovalDto | null>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    return api
      .dockerContents()
      .then((loaded) => {
        setDocker(loaded);
        setSelection(initialDockerSelection(loaded.items));
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const items = useMemo(() => docker?.items ?? [], [docker]);
  const picked = useMemo(
    () => dockerSelected(selection, items),
    [selection, items],
  );

  if (docker?.status === "missing") {
    return null;
  }

  const remove = () => {
    setBusy(true);
    setError(null);
    api
      .removeDocker(
        picked.items.map(({ kind, reference }) => ({ kind, reference })),
      )
      .then((done) => {
        setResult(done);
        setConfirming(false);
        if (done.removed.length > 0) {
          void requestRescan();
        }
        return load();
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  const total = items.reduce((sum, item) => sum + item.bytes, 0);
  const unused = items
    .filter((item) => !item.inUse)
    .reduce((sum, item) => sum + item.bytes, 0);

  return (
    <section className="dupes-screen" aria-labelledby="docker-title">
      <h2 id="docker-title" className="cleanup-section-title">
        Docker
        <InfoTip label="Docker">
          Everything Docker keeps (images, containers, volumes and build cache)
          lives inside one disk image. StorageLens never deletes that file; it
          asks Docker to remove what nothing uses. Docker deletes for good: none
          of this goes to the Trash.
        </InfoTip>
      </h2>
      <div className="apps-toolbar">
        <span
          className={loading ? "screen-status loading" : "screen-status"}
          aria-live="polite"
        >
          {loading
            ? "Asking Docker what it holds…"
            : docker?.status === "ready"
              ? `${formatBytes(total)} inside Docker · ${formatBytes(unused)} unused`
              : null}
        </span>
        <button
          type="button"
          className="button button-secondary"
          disabled={loading || busy}
          onClick={() => void load()}
        >
          Refresh
        </button>
      </div>

      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}
      {docker?.status === "unavailable" ? (
        <p className="empty-text">
          Docker is not running, so what it holds cannot be listed. Start
          Docker Desktop and refresh. ({docker.message})
        </p>
      ) : null}
      {docker?.status === "ready" && items.length === 0 ? (
        <p className="empty-text">Docker holds nothing.</p>
      ) : null}

      {result !== null ? (
        <section className="cleanup-result" aria-label="Docker removal result">
          <p className="cleanup-counts">
            Removed: {formatCount(result.removed.length)}
            {result.failed.length > 0
              ? ` · Not removed: ${formatCount(result.failed.length)}`
              : ""}
          </p>
          {result.failed.length > 0 ? (
            <ul className="cleanup-reasons">
              {result.failed.map((failure) => (
                <li key={failure.reference}>
                  <span className="rule-path" title={failure.reference}>
                    {failure.reference}
                  </span>
                  <span>{failure.reason}</span>
                </li>
              ))}
            </ul>
          ) : null}
          <p className="empty-text">
            Docker hands the space back to the disk within a few minutes.
            Rescan to see it.
          </p>
        </section>
      ) : null}

      {DOCKER_KINDS.map(({ kind, title, hint }) => {
        const rows = items.filter((item) => item.kind === kind);
        if (rows.length === 0) {
          return null;
        }
        return (
          <section
            key={kind}
            className="dupes-group"
            aria-labelledby={`docker-${kind}`}
          >
            <header className="dupes-group-head">
              <h3 id={`docker-${kind}`} className="dupes-group-title">
                {title}
                <InfoTip label={title}>{hint}</InfoTip>
              </h3>
              <span className="dupes-reclaim">
                {formatBytes(rows.reduce((sum, row) => sum + row.bytes, 0))}
              </span>
            </header>
            <ul className="dupes-files">
              {rows.map((row) => (
                <DockerRow
                  key={dockerKey(row)}
                  item={row}
                  ticked={selection.has(dockerKey(row))}
                  disabled={busy}
                  onToggle={() =>
                    setSelection((current) => toggleDocker(current, row))
                  }
                />
              ))}
            </ul>
          </section>
        );
      })}

      {picked.items.length > 0 ? (
        <footer className="cleanup-footer">
          <span className="cleanup-total" aria-live="polite">
            {confirming
              ? `Remove ${formatCount(picked.items.length)} ${
                  picked.items.length === 1 ? "item" : "items"
                } (${formatBytes(picked.bytes)}) for good? This cannot be undone.`
              : `${formatCount(picked.items.length)} selected · ${formatBytes(
                  picked.bytes,
                )}`}
          </span>
          {confirming ? (
            <span className="apps-remove-actions">
              <button
                type="button"
                className="button button-secondary"
                disabled={busy}
                onClick={() => setConfirming(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                className="button button-danger"
                disabled={busy}
                aria-busy={busy}
                onClick={remove}
              >
                {busy ? "Removing…" : "Remove for good"}
              </button>
            </span>
          ) : (
            <button
              type="button"
              className="button"
              onClick={() => {
                setResult(null);
                setConfirming(true);
              }}
            >
              Remove from Docker…
            </button>
          )}
        </footer>
      ) : null}
    </section>
  );
}

function DockerRow({
  item,
  ticked,
  disabled,
  onToggle,
}: {
  readonly item: DockerItemDto;
  readonly ticked: boolean;
  readonly disabled: boolean;
  readonly onToggle: () => void;
}) {
  const when =
    item.when === null
      ? null
      : `${item.kind === "build" ? "Last used" : "Created"} ${formatDate(item.when)}`;
  return (
    <li className="dupes-file" data-ticked={ticked}>
      <label title={item.inUse ? "In use: Docker would refuse" : undefined}>
        <input
          type="checkbox"
          checked={ticked}
          disabled={item.inUse || disabled}
          onChange={onToggle}
        />{" "}
        <span className="docker-item">
          <span className="rule-path" title={item.reference}>
            {item.name}
          </span>
          <span className="dupes-meta">
            {item.inUse ? "In use · " : ""}
            {item.detail}
          </span>
        </span>
      </label>
      <span className="dupes-meta">{when}</span>
      <span className="rule-item-bytes">{formatBytes(item.bytes)}</span>
    </li>
  );
}
