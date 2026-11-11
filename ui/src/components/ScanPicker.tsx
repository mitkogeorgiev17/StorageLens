/**
 * Choose what to scan: the home folder, a mounted volume, a folder from the
 * OS's own picker (NSOpenPanel / IFileOpenDialog, via the dialog plugin, #72),
 * or a typed absolute path. Cancelling the picker changes nothing.
 *
 * On macOS, a denied Full Disk Access probe shows a banner first: without it,
 * protected folders are skipped and the totals come out low.
 */

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useId, useState, type FormEvent } from "react";

import { api, type DiskAccess, type ScanTargetDto } from "../lib/api";
import { formatBytes } from "../lib/format";
import { useAppStore } from "../store";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export interface ScanPickerProps {
  /** Called once a scan has started. */
  readonly onStarted?: () => void;
}

export default function ScanPicker({ onStarted }: ScanPickerProps) {
  const scanStarted = useAppStore((state) => state.scanStarted);
  const navigate = useAppStore((state) => state.navigate);
  const [targets, setTargets] = useState<readonly ScanTargetDto[] | null>(null);
  const [access, setAccess] = useState<DiskAccess | null>(null);
  const [path, setPath] = useState("");
  const [starting, setStarting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pathId = useId();
  const errorId = useId();

  useEffect(() => {
    let live = true;
    api
      .listScanTargets()
      .then((loaded) => live && setTargets(loaded))
      .catch((cause: unknown) => {
        if (live) {
          setTargets([]);
          setError(message(cause));
        }
      });
    api
      .checkFullDiskAccess()
      .then((result) => live && setAccess(result))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

  const start = async (roots: readonly string[]) => {
    const paths = roots.map((root) => root.trim());
    if (paths.length === 0 || paths.some((root) => root === "")) {
      setError("Type the full path of a folder to scan.");
      return;
    }
    const key = paths.join(", ");
    setStarting(key);
    setError(null);
    try {
      // One target may cover several drives (#195).
      const scanId = await api.startScan(paths);
      scanStarted(scanId, key);
      navigate("overview");
      onStarted?.();
    } catch (cause: unknown) {
      setError(message(cause));
    } finally {
      setStarting(null);
    }
  };

  const choose = async () => {
    setError(null);
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: "Choose a folder to scan",
      });
      // `null` when the picker was cancelled: leave everything as it was.
      if (typeof picked === "string") {
        setPath(picked);
        await start([picked]);
      }
    } catch (cause: unknown) {
      setError(message(cause));
    }
  };

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void start([path]);
  };

  const busy = starting !== null;

  return (
    <div className="picker">
      {access === "denied" ? (
        <div className="banner" role="note">
          <p>
            <strong>Full Disk Access is off.</strong> Mail, Messages, Safari data and other
            protected folders will be skipped, so totals will be low. A whole-disk scan needs it
            most.
          </p>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => {
              api.openFullDiskAccessSettings().catch((cause: unknown) => setError(message(cause)));
            }}
          >
            Open Privacy settings
          </button>
        </div>
      ) : null}

      {targets === null ? <p className="loading">Looking for volumes…</p> : null}
      {targets !== null && targets.length > 0 ? (
        <ul className="targets" aria-label="Scan targets">
          {targets.map((target) => {
            const key = target.roots.join(", ");
            return (
              <li key={`${target.kind}:${key}`}>
                <button
                  type="button"
                  className="target"
                  data-startup={target.startup}
                  disabled={busy}
                  aria-busy={starting === key}
                  onClick={() => void start(target.roots)}
                >
                  <span className="target-label">
                    {target.label}
                    {target.kind === "everything" ? (
                      <span className="target-kind"> · all fixed drives, recommended</span>
                    ) : null}
                    {target.kind === "home" ? <span className="target-kind"> · home folder only</span> : null}
                    {target.removable ? <span className="target-kind"> · removable</span> : null}
                  </span>
                  <span className="target-path">{key}</span>
                  {target.total !== null && target.available !== null ? (
                    <span className="target-meta">
                      {formatBytes(target.total - target.available)} used of{" "}
                      {formatBytes(target.total)}
                    </span>
                  ) : null}
                  <span className="target-action">{starting === key ? "Starting…" : "Scan"}</span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}

      <div className="choose-row">
        <button
          type="button"
          className="button button-secondary"
          disabled={busy}
          onClick={() => void choose()}
        >
          Choose folder…
        </button>
      </div>

      <form className="path-form" onSubmit={onSubmit}>
        <label htmlFor={pathId}>Or type a folder's path</label>
        <div className="path-row">
          <input
            id={pathId}
            className="input"
            type="text"
            spellCheck={false}
            autoComplete="off"
            placeholder={navigator.userAgent.includes("Windows") ? "D:\\Projects" : "/Users/you/Projects"}
            value={path}
            aria-invalid={error !== null}
            aria-describedby={error === null ? undefined : errorId}
            onChange={(event) => setPath(event.target.value)}
          />
          <button type="submit" className="button" disabled={busy} aria-busy={busy && starting === path.trim()}>
            {busy && starting === path.trim() ? "Starting…" : "Scan"}
          </button>
        </div>
      </form>

      {error !== null ? (
        <p id={errorId} className="picker-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
