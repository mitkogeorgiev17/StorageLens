/**
 * Apps (#38): "which app is eating 40 GB, and do I still use it?"
 *
 * A sortable table of installed apps — app size, associated data, total, last
 * used, status — with system apps excluded entirely (the backend never sends
 * them). Associated data is a heuristic and always labelled "estimated".
 *
 * Removal goes through the same receipt → Trash → undo path as Cleanup: the
 * backend builds the plan and `execute_deletion` runs it behind the safety
 * guard and preflight re-stat. A running app needs an explicit "remove
 * anyway", which the backend checks too. A running app can be force quit
 * (#113) after a confirmation: unsaved work in it is lost.
 *
 * An app with its own uninstaller (#115) is uninstalled with it: on Windows
 * the command "Add or remove apps" runs (moving the folder would leave its
 * registry entries, services and shortcuts behind), on macOS a vendor
 * uninstaller. Moving a Mac app to the Trash is the standard uninstall
 * otherwise. Once the app is gone from the system's list (#120), what it left
 * behind is offered for the Trash, strong matches ticked.
 */

import { Fragment, useCallback, useEffect, useMemo, useState } from "react";

import Leftovers from "../components/Leftovers";
import Receipt from "../components/Receipt";
import {
  api,
  type AppRowDto,
  type DeletionPlanDto,
  type DeletionResultDto,
  type LeftoverDto,
} from "../lib/api";
import {
  STATUS_LABELS,
  nextSort,
  removalWarnings,
  sortApps,
  type AppSort,
  type AppSortKey,
} from "../lib/apps";
import { formatBytes, formatCount, formatDate } from "../lib/format";
import { afterTrash } from "../lib/rescan";
import { useAppStore } from "../store";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const IS_MAC =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

/** Microsoft Store apps (#122): removed by Windows itself. */
const isStoreApp = (row: AppRowDto) => row.id.startsWith("msix:");

const COLUMNS: readonly { key: AppSortKey; label: string; numeric: boolean }[] =
  [
    { key: "name", label: "Name", numeric: false },
    { key: "appBytes", label: "App", numeric: true },
    { key: "satelliteBytes", label: "Data (estimated)", numeric: true },
    { key: "totalBytes", label: "Total", numeric: true },
    { key: "lastUsed", label: "Last used", numeric: false },
    { key: "status", label: "Status", numeric: false },
  ];

export default function AppsScreen() {
  const scan = useAppStore((state) => state.scan);
  const scanId = scan.kind === "done" ? scan.summary.scanId : null;
  const [rows, setRows] = useState<readonly AppRowDto[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [sort, setSort] = useState<AppSort>({
    key: "totalBytes",
    descending: true,
  });
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const [removing, setRemoving] = useState<AppRowDto | null>(null);
  const [quitting, setQuitting] = useState<AppRowDto | null>(null);
  const [quitNote, setQuitNote] = useState<string | null>(null);
  const [uninstalling, setUninstalling] = useState<AppRowDto | null>(null);
  const [waiting, setWaiting] = useState<AppRowDto | null>(null);
  /** The app whose bundle the open receipt moves to the Trash. */
  const [trashing, setTrashing] = useState<AppRowDto | null>(null);
  const [leftovers, setLeftovers] = useState<{
    readonly row: AppRowDto;
    readonly items: readonly LeftoverDto[];
  } | null>(null);
  const [plan, setPlan] = useState<DeletionPlanDto | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback((refresh: boolean) => {
    setLoading(true);
    setError(null);
    return api
      .listApps(refresh)
      .then(setRows)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, []);

  // Again after each rescan: a move to the Trash or an undo changes the list.
  useEffect(() => {
    void load(false);
  }, [load, scanId]);

  const sorted = useMemo(() => sortApps(rows ?? [], sort), [rows, sort]);

  const toggle = (id: string) =>
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(id)) {
        next.add(id);
      }
      return next;
    });

  const review = (
    row: AppRowDto,
    includeData: boolean,
    allowRunning: boolean,
  ) => {
    if (scanId === null) {
      return;
    }
    setBusy(true);
    setError(null);
    api
      .previewAppRemoval(scanId, row.path, includeData, allowRunning)
      .then((receipt) => {
        setRemoving(null);
        setTrashing(row);
        setPlan(receipt);
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  // Poll until the uninstaller has removed the app from the system's list,
  // then show what it left behind.
  useEffect(() => {
    if (waiting === null) {
      return;
    }
    let live = true;
    const timer = window.setInterval(() => {
      api
        .uninstallFinished(waiting.id)
        .then((done) => (done ? api.appLeftovers(waiting.id) : null))
        .then((items) => {
          if (live && items !== null) {
            setWaiting(null);
            setLeftovers({ row: waiting, items });
          }
        })
        .catch((cause: unknown) => {
          if (live) {
            setWaiting(null);
            setError(message(cause));
          }
        });
    }, 2000);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [waiting]);

  const reviewLeftovers = (row: AppRowDto, paths: readonly string[]) => {
    if (scanId === null) {
      return;
    }
    setBusy(true);
    setError(null);
    api
      .previewLeftoverRemoval(scanId, row.id, paths)
      .then(setPlan)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  const leftoversDone = () => {
    setLeftovers(null);
    // The app is gone from the machine: read the inventory again.
    void load(true);
  };

  const finished = (moved: DeletionResultDto) => {
    setPlan(null);
    afterTrash(moved);
    if (leftovers !== null) {
      leftoversDone();
      return;
    }
    // The bundle is in the Trash: show what it still leaves (data not chosen,
    // system helpers, receipts), if anything (#123).
    if (trashing !== null) {
      const row = trashing;
      setTrashing(null);
      api
        .appLeftovers(row.id)
        .then((items) => {
          if (items.length > 0) {
            setLeftovers({ row, items });
          }
        })
        .catch((cause: unknown) => setError(message(cause)));
    }
    void load(false);
  };

  const uninstall = (row: AppRowDto) => {
    setBusy(true);
    setError(null);
    api
      .uninstallApp(row.id)
      .then(() => {
        setUninstalling(null);
        setLeftovers(null);
        setWaiting(row);
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  const forceQuit = (row: AppRowDto) => {
    setBusy(true);
    setError(null);
    api
      .forceQuitApp(row.path)
      .then(({ stopped, failed }) => {
        setQuitting(null);
        setQuitNote(
          failed > 0
            ? `${row.name}: ${formatCount(failed)} ${failed === 1 ? "process" : "processes"} could not be stopped (another user's, or protected).`
            : stopped === 0
              ? `${row.name} was no longer running.`
              : `${row.name} was force quit.`,
        );
        return load(false);
      })
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="apps-screen">
      <div className="apps-toolbar">
        <span className="screen-status" aria-live="polite">
          {loading
            ? "Reading installed apps and measuring their data… this takes a few seconds."
            : rows !== null
              ? `${formatCount(rows.length)} apps · ${formatBytes(
                  rows.reduce((sum, row) => sum + row.totalBytes, 0),
                )}`
              : ""}
        </span>
        <button
          type="button"
          className="button button-secondary"
          disabled={loading}
          onClick={() => void load(true)}
        >
          Refresh
        </button>
      </div>

      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}
      {scanId === null ? (
        <p className="map-notice">
          Removing an app needs a finished scan of the disk it is on. Start one
          from the Overview.
        </p>
      ) : null}

      {quitNote !== null ? (
        <p className="screen-status" role="status">
          {quitNote}
        </p>
      ) : null}

      {waiting !== null ? (
        <section className="apps-remove" aria-live="polite">
          <h2>Waiting for {waiting.name}'s uninstaller…</h2>
          <p>
            Follow its steps. Once {waiting.name} is gone, what it left behind
            is listed here.
          </p>
          <div className="apps-remove-actions">
            <button
              type="button"
              className="button button-secondary"
              onClick={() => {
                setWaiting(null);
                setQuitNote(
                  `Stopped waiting for ${waiting.name}. Press Refresh once its uninstaller finishes.`,
                );
              }}
            >
              Stop waiting
            </button>
          </div>
        </section>
      ) : null}

      {leftovers !== null ? (
        <Leftovers
          key={leftovers.row.id}
          row={leftovers.row}
          items={leftovers.items}
          busy={busy}
          canReview={scanId !== null}
          onReview={(paths) => reviewLeftovers(leftovers.row, paths)}
          onDone={leftoversDone}
        />
      ) : null}

      {uninstalling !== null ? (
        <section className="apps-remove" aria-labelledby="apps-uninstall-title">
          <h2 id="apps-uninstall-title">Uninstall {uninstalling.name}?</h2>
          <p>
            {isStoreApp(uninstalling)
              ? `Windows removes ${uninstalling.name} and its app data, as Settings → Apps does. This takes a few seconds.`
              : IS_MAC
                ? `Opens ${uninstalling.name}'s own uninstaller. It also removes helpers and background items that moving the app to the Trash leaves behind.`
                : `Runs ${uninstalling.name}'s own uninstaller, the same one Settings → Apps → Installed apps runs. Windows may ask for administrator permission.`}
          </p>
          <div className="apps-remove-actions">
            <button
              type="button"
              className="button"
              disabled={busy}
              onClick={() => uninstall(uninstalling)}
            >
              {isStoreApp(uninstalling)
                ? busy
                  ? "Uninstalling…"
                  : "Uninstall"
                : IS_MAC
                  ? "Open uninstaller"
                  : "Run uninstaller"}
            </button>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => setUninstalling(null)}
            >
              Cancel
            </button>
          </div>
        </section>
      ) : null}

      {quitting !== null ? (
        <section className="apps-remove" aria-labelledby="apps-quit-title">
          <h2 id="apps-quit-title">Force quit {quitting.name}?</h2>
          <p>
            Every process of the app stops at once. Unsaved work in it is lost.
          </p>
          <div className="apps-remove-actions">
            <button
              type="button"
              className="button button-danger"
              disabled={busy}
              onClick={() => forceQuit(quitting)}
            >
              Force quit
            </button>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => setQuitting(null)}
            >
              Cancel
            </button>
          </div>
        </section>
      ) : null}

      {removing !== null ? (
        <RemovePanel
          row={removing}
          busy={busy}
          onCancel={() => setRemoving(null)}
          onReview={(includeData, allowRunning) =>
            review(removing, includeData, allowRunning)
          }
        />
      ) : null}

      {rows !== null && rows.length === 0 ? (
        <section className="empty" aria-labelledby="apps-none">
          <h2 id="apps-none">No apps found</h2>
          <p className="empty-text">
            No installed apps were found outside the system ones.
          </p>
        </section>
      ) : null}

      {sorted.length > 0 ? (
        <table className="apps-table">
          <thead>
            <tr>
              {COLUMNS.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  className={column.numeric ? "apps-num" : undefined}
                  aria-sort={
                    sort.key === column.key
                      ? sort.descending
                        ? "descending"
                        : "ascending"
                      : "none"
                  }
                >
                  <button
                    type="button"
                    className="apps-sort"
                    onClick={() =>
                      setSort((current) => nextSort(current, column.key))
                    }
                  >
                    {column.label}
                    {sort.key === column.key
                      ? sort.descending
                        ? " ↓"
                        : " ↑"
                      : ""}
                  </button>
                </th>
              ))}
              <th scope="col">
                <span className="visually-hidden">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((row) => {
              const open = expanded.has(row.id);
              const removable = row.path !== "" && scanId !== null;
              return (
                <Fragment key={row.id}>
                  <tr>
                    <td>
                      <button
                        type="button"
                        className="apps-expand"
                        aria-expanded={open}
                        aria-controls={`app-data-${row.id}`}
                        disabled={row.satellites.length === 0}
                        onClick={() => toggle(row.id)}
                        title={
                          row.satellites.length === 0
                            ? "No associated data found"
                            : "Show associated data"
                        }
                      >
                        {open ? "▾" : "▸"}
                      </button>
                      <span className="apps-name" title={row.path}>
                        {row.name}
                      </span>
                      {row.version !== null ? (
                        <span className="apps-version">{row.version}</span>
                      ) : null}
                      {isStoreApp(row) ? (
                        <span className="apps-flag">Store</span>
                      ) : null}
                      {row.running ? (
                        <span className="apps-flag">Running</span>
                      ) : null}
                    </td>
                    <td className="apps-num">{formatBytes(row.appBytes)}</td>
                    <td className="apps-num">
                      {row.satellites.length === 0
                        ? "—"
                        : `~${formatBytes(row.satelliteBytes)}`}
                    </td>
                    <td className="apps-num">{formatBytes(row.totalBytes)}</td>
                    <td>{formatDate(row.lastUsed)}</td>
                    <td>
                      <span className="app-status" data-status={row.status}>
                        {STATUS_LABELS[row.status]}
                      </span>
                    </td>
                    <td className="apps-actions">
                      {row.running ? (
                        <button
                          type="button"
                          className="link-button"
                          disabled={busy}
                          onClick={() => {
                            setQuitNote(null);
                            setQuitting(row);
                          }}
                        >
                          Force quit…
                        </button>
                      ) : null}
                      {row.hasUninstaller ? (
                        <button
                          type="button"
                          className="link-button"
                          disabled={busy}
                          onClick={() => {
                            setQuitNote(null);
                            setUninstalling(row);
                          }}
                        >
                          Uninstall…
                        </button>
                      ) : null}
                      {/* On Windows the registered uninstaller is the way;
                          moving the folder is only offered without one. */}
                      {IS_MAC || !row.hasUninstaller ? (
                        <button
                          type="button"
                          className="link-button"
                          disabled={!removable || busy}
                          title={
                            row.path === ""
                              ? "No install folder recorded. Use the system uninstaller."
                              : scanId === null
                                ? "Scan the disk first"
                                : IS_MAC
                                  ? undefined
                                  : "No uninstaller registered: moves the install folder to the Recycle Bin"
                          }
                          onClick={() => setRemoving(row)}
                        >
                          Remove…
                        </button>
                      ) : null}
                    </td>
                  </tr>
                  {open ? (
                    <tr id={`app-data-${row.id}`} className="apps-detail">
                      <td colSpan={COLUMNS.length + 1}>
                        <p className="apps-detail-title">
                          Associated data (estimated). Matched by name, so check
                          before removing.
                        </p>
                        <ul className="apps-satellites">
                          {row.satellites.map((satellite) => (
                            <li key={satellite.path}>
                              <span
                                className="rule-path"
                                title={satellite.path}
                              >
                                {satellite.path}
                              </span>
                              <span>~{formatBytes(satellite.bytes)}</span>
                            </li>
                          ))}
                        </ul>
                      </td>
                    </tr>
                  ) : null}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      ) : null}

      {plan !== null ? (
        <Receipt
          plan={plan}
          onCancel={() => {
            setPlan(null);
            setTrashing(null);
          }}
          onDone={finished}
        />
      ) : null}
    </div>
  );
}

function RemovePanel({
  row,
  busy,
  onCancel,
  onReview,
}: {
  readonly row: AppRowDto;
  readonly busy: boolean;
  readonly onCancel: () => void;
  readonly onReview: (includeData: boolean, allowRunning: boolean) => void;
}) {
  const [includeData, setIncludeData] = useState(false);
  const [allowRunning, setAllowRunning] = useState(false);
  const warnings = removalWarnings(row);
  const hasData = row.satellites.length > 0;
  return (
    <section className="apps-remove" aria-labelledby="apps-remove-title">
      <h2 id="apps-remove-title">Remove {row.name}</h2>
      {warnings.map((warning) => (
        <p key={warning} className="map-notice" role="alert">
          {warning}
        </p>
      ))}
      <fieldset className="apps-remove-options">
        <legend className="visually-hidden">What to remove</legend>
        <label>
          <input
            type="radio"
            name="apps-remove-scope"
            checked={!includeData}
            onChange={() => setIncludeData(false)}
          />{" "}
          App only · {formatBytes(row.appBytes)}
        </label>
        <label>
          <input
            type="radio"
            name="apps-remove-scope"
            checked={includeData}
            disabled={!hasData}
            onChange={() => setIncludeData(true)}
          />{" "}
          App and associated data (estimated) · {formatBytes(row.totalBytes)}
        </label>
      </fieldset>
      {row.running ? (
        <label className="apps-remove-confirm">
          <input
            type="checkbox"
            checked={allowRunning}
            onChange={(event) => setAllowRunning(event.target.checked)}
          />{" "}
          Remove it anyway while it is running
        </label>
      ) : null}
      <p className="empty-text">
        Everything goes to the Trash and can be undone. The next step lists
        every path before anything moves.
      </p>
      <div className="apps-remove-actions">
        <button
          type="button"
          className="button button-secondary"
          onClick={onCancel}
        >
          Cancel
        </button>
        <button
          type="button"
          className="button"
          disabled={busy || (row.running && !allowRunning)}
          onClick={() => onReview(includeData, allowRunning)}
        >
          Review and move to Trash…
        </button>
      </div>
    </section>
  );
}
