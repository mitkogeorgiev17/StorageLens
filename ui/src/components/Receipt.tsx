/**
 * The dry-run receipt (#34, PLAN §15.2): every path, size, and risk that will
 * move, before anything does. Nothing moves until "Move to Trash" is clicked
 * on this sheet; over 5 GB that click is asked for twice (PLAN §15.7).
 *
 * While the move runs, "Stop" ends it before the next item: what moved stays
 * moved (and can be undone), the rest is reported as not reached.
 */

import { useEffect, useRef, useState } from "react";
import { List, type RowComponentProps } from "react-window";

import {
  api,
  onCleanupProgress,
  type DeletionPlanDto,
  type DeletionResultDto,
} from "../lib/api";
import { RISK_LABELS } from "../lib/cleanup";
import { formatBytes, formatCount } from "../lib/format";

const ROW_HEIGHT = 32;
/** Rows visible before the list scrolls. */
const VISIBLE_ROWS = 10;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function Line({
  index,
  style,
  plan,
}: RowComponentProps<{ plan: DeletionPlanDto }>) {
  const item = plan.items[index];
  if (item === undefined) {
    return null;
  }
  return (
    <div className="receipt-row" style={style} role="row">
      <span className="receipt-path" role="cell" title={item.path}>
        {item.path}
      </span>
      <span className="risk-badge" data-risk={item.risk} role="cell">
        {RISK_LABELS[item.risk]}
      </span>
      <span className="receipt-bytes" role="cell">
        {formatBytes(item.bytes)}
      </span>
    </div>
  );
}

export default function Receipt({
  plan,
  onCancel,
  onDone,
}: {
  readonly plan: DeletionPlanDto;
  readonly onCancel: () => void;
  readonly onDone: (result: DeletionResultDto) => void;
}) {
  const dialog = useRef<HTMLDialogElement | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [done, setDone] = useState(0);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const element = dialog.current;
    if (element !== null && !element.open) {
      element.showModal();
    }
    return () => element?.close();
  }, []);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let live = true;
    void onCleanupProgress((event) => {
      if (event.planId === plan.planId) {
        setDone(event.done);
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
  }, [plan.planId]);

  const total = formatBytes(plan.totalBytes);
  const count = plan.items.length;

  const move = () => {
    if (plan.needsConfirmation && !confirming) {
      setConfirming(true);
      return;
    }
    setRunning(true);
    setError(null);
    api
      .executeDeletion(plan.planId, plan.needsConfirmation)
      .then(onDone)
      .catch((cause: unknown) => {
        setRunning(false);
        setError(message(cause));
      });
  };

  const stop = () => {
    setStopping(true);
    api.cancelDeletion().catch((cause: unknown) => setError(message(cause)));
  };

  return (
    <dialog
      ref={dialog}
      className="receipt"
      aria-labelledby="receipt-title"
      onCancel={(event) => {
        // Esc closes the sheet only while nothing is moving.
        event.preventDefault();
        if (!running) {
          onCancel();
        }
      }}
    >
      <h2 id="receipt-title" className="receipt-title">
        Move {formatCount(count)} {count === 1 ? "item" : "items"} ({total}) to
        the Trash?
      </h2>
      <p className="receipt-note">
        Everything below goes to the Trash, not away for good: you can undo it
        here, or put items back from the Trash yourself. Each item is checked
        again just before it moves; anything that changed since the scan is
        skipped and listed.
      </p>
      <div className="receipt-table" role="table" aria-label="Items to move">
        <List
          rowComponent={Line}
          rowCount={count}
          rowHeight={ROW_HEIGHT}
          rowProps={{ plan }}
          style={{ height: Math.min(count, VISIBLE_ROWS) * ROW_HEIGHT }}
        />
      </div>
      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}
      {running ? (
        <div className="receipt-progress" role="status" aria-live="polite">
          <progress max={count} value={done} />
          <span>
            {stopping
              ? "Stopping after the current item…"
              : `Moving ${formatCount(Math.min(done + 1, count))} of ${formatCount(count)}…`}
          </span>
        </div>
      ) : null}
      {confirming && !running ? (
        <p className="receipt-confirm" role="alert">
          This is more than 5 GB. Click once more to move {total} to the Trash.
        </p>
      ) : null}
      <div className="receipt-actions">
        {running ? (
          <button
            type="button"
            className="button button-secondary"
            onClick={stop}
            disabled={stopping}
          >
            Stop
          </button>
        ) : (
          <button
            type="button"
            className="button button-secondary"
            onClick={onCancel}
          >
            Cancel
          </button>
        )}
        <button
          type="button"
          className="button button-danger"
          onClick={move}
          disabled={running}
          aria-busy={running}
        >
          {confirming ? `Yes, move ${total}` : `Move to Trash`}
        </button>
      </div>
    </dialog>
  );
}
