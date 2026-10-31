/**
 * Choose what to scan: the home folder, a mounted volume, or a typed absolute
 * path. No native folder dialog — that needs a Tauri plugin (a new
 * dependency, AGENTS.md §2.3), so it waits for a decision.
 *
 * On macOS, a denied Full Disk Access probe shows a banner first: without it,
 * protected folders are skipped and the totals come out low.
 */

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

  const start = async (root: string) => {
    const trimmed = root.trim();
    if (trimmed === "") {
      setError("Type the full path of a folder to scan.");
      return;
    }
    setStarting(trimmed);
    setError(null);
    try {
      const scanId = await api.startScan(trimmed);
      scanStarted(scanId, trimmed);
      navigate("overview");
      onStarted?.();
    } catch (cause: unknown) {
      setError(message(cause));
    } finally {
      setStarting(null);
    }
  };

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void start(path);
  };

  const busy = starting !== null;

  return (
    <div className="picker">
      {access === "denied" ? (
        <div className="banner" role="note">
          <p>
            <strong>Full Disk Access is off.</strong> Mail, Messages, Safari data and other
            protected folders will be skipped, so totals will be low.
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
          {targets.map((target) => (
            <li key={`${target.kind}:${target.path}`}>
              <button
                type="button"
                className="target"
                disabled={busy}
                aria-busy={starting === target.path}
                onClick={() => void start(target.path)}
              >
                <span className="target-label">
                  {target.label}
                  {target.kind === "home" ? <span className="target-kind"> · home folder</span> : null}
                  {target.removable ? <span className="target-kind"> · removable</span> : null}
                </span>
                <span className="target-path">{target.path}</span>
                {target.total !== null && target.available !== null ? (
                  <span className="target-meta">
                    {formatBytes(target.total - target.available)} used of{" "}
                    {formatBytes(target.total)}
                  </span>
                ) : null}
                <span className="target-action">{starting === target.path ? "Starting…" : "Scan"}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      <form className="path-form" onSubmit={onSubmit}>
        <label htmlFor={pathId}>Or scan a folder by path</label>
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
          <button type="submit" className="button" disabled={busy}>
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
