/**
 * The running scan: live counters from `scan://progress` (throttled to 10 per
 * second by the backend) and a Cancel button. A cancelled scan ends as
 * `cancelled` with whatever it counted.
 */

import { useState } from "react";

import { api } from "../lib/api";
import { formatBytes, formatCount, formatDuration } from "../lib/format";
import type { ScanState } from "../store";

export default function ScanProgress({
  scan,
}: {
  readonly scan: Extract<ScanState, { kind: "scanning" }>;
}) {
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { progress } = scan;
  const seconds = progress.elapsedMs / 1000;
  const rate = seconds > 0 ? progress.files / seconds : 0;

  const cancel = () => {
    setCancelling(true);
    setError(null);
    api.cancelScan(scan.scanId).catch((cause: unknown) => {
      setCancelling(false);
      setError(cause instanceof Error ? cause.message : String(cause));
    });
  };

  return (
    <section className="card progress" aria-labelledby="progress-title">
      <div className="card-heading">
        <h2 id="progress-title" className="card-title">
          Scanning {scan.root}
        </h2>
        <button
          type="button"
          className="button button-secondary"
          disabled={cancelling}
          onClick={cancel}
        >
          {cancelling ? "Cancelling…" : "Cancel"}
        </button>
      </div>
      <div className="progress-bar" aria-hidden="true">
        <span />
      </div>
      <dl className="progress-stats">
        <div>
          <dt>Files</dt>
          <dd>{formatCount(progress.files)}</dd>
        </div>
        <div>
          <dt>Folders</dt>
          <dd>{formatCount(progress.dirs)}</dd>
        </div>
        <div>
          <dt>Space</dt>
          <dd>{formatBytes(progress.bytesAlloc)}</dd>
        </div>
        <div>
          <dt>Elapsed</dt>
          <dd>{formatDuration(progress.elapsedMs)}</dd>
        </div>
        <div>
          <dt>Rate</dt>
          <dd>{formatCount(Math.round(rate))} files/s</dd>
        </div>
      </dl>
      <p className="progress-path" title={progress.currentPath ?? undefined}>
        {progress.currentPath ?? "Starting…"}
      </p>
      {error !== null ? (
        <p className="picker-error" role="alert">
          {error}
        </p>
      ) : null}
    </section>
  );
}
