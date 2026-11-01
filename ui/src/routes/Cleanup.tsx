/**
 * Cleanup (#34): the screen that earns trust.
 *
 * Rule cards grouped by kind, each with its explanation and risk; a running
 * total of what is selected; a receipt listing every path before anything
 * moves; then, item by item, what moved, what was skipped and why — with
 * undo. Duplicate files are a section at the end (#112). Every move goes to the Trash through the safety crate's guard and
 * preflight re-stat; nothing here deletes anything itself.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import DuplicatesSection from "../components/DuplicatesSection";
import Receipt from "../components/Receipt";
import Skeleton from "../components/Skeleton";
import RuleCard from "../components/RuleCard";
import {
  api,
  type CleanupDto,
  type DeletionPlanDto,
  type DeletionResultDto,
  type ItemStatus,
  type UndoOutcome,
  type UndoResultDto,
} from "../lib/api";
import {
  KIND_LABELS,
  KIND_ORDER,
  STATUS_LABELS,
  UNDO_LABELS,
  countBy,
  initialSelection,
  refusedForPermission,
  selectionTotal,
  sizeIndex,
  toggleGroup,
  toggleItem,
  topLevel,
  type Selection,
} from "../lib/cleanup";
import { afterTrash } from "../lib/rescan";
import { formatBytes, formatCount } from "../lib/format";
import { useAppStore } from "../store";

const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const STATUS_ORDER: readonly ItemStatus[] = [
  "trashed",
  "skipped",
  "refused",
  "failed",
  "cancelled",
];
const UNDO_ORDER: readonly UndoOutcome[] = [
  "restored",
  "already-restored",
  "not-in-trash",
  "occupied",
  "failed",
  "not-trashed",
];

export default function CleanupScreen() {
  const scan = useAppStore((state) => state.scan);
  const scanId = scan.kind === "done" ? scan.summary.scanId : null;
  const [cleanup, setCleanup] = useState<CleanupDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<Selection>(new Set());
  const [plan, setPlan] = useState<DeletionPlanDto | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback((id: number) => {
    setError(null);
    return api
      .getRules(id)
      .then((loaded) => {
        setCleanup(loaded);
        setSelection(initialSelection(loaded.groups));
      })
      .catch((cause: unknown) => setError(message(cause)));
  }, []);

  useEffect(() => {
    setCleanup(null);
    if (scanId !== null) {
      void load(scanId);
    }
  }, [scanId, load]);

  const sizes = useMemo(() => sizeIndex(cleanup?.groups ?? []), [cleanup]);
  const total = useMemo(
    () => selectionTotal(selection, sizes),
    [selection, sizes],
  );

  if (scanId === null) {
    return (
      <section className="empty" aria-labelledby="cleanup-empty">
        <h2 id="cleanup-empty">Nothing scanned yet</h2>
        <p className="empty-text">
          Cleanup suggestions appear after a scan. Start one from the Overview.
        </p>
      </section>
    );
  }

  const review = () => {
    setBusy(true);
    setError(null);
    api
      .previewDeletion(scanId, topLevel(selection))
      .then(setPlan)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setBusy(false));
  };

  // The result and its undo live above every screen (App, #97); the rescan
  // that follows replaces this list.
  const finished = (moved: DeletionResultDto) => {
    setPlan(null);
    afterTrash(moved);
  };

  const groups = cleanup?.groups ?? [];
  return (
    <div className="cleanup-screen">
      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}

      <div className="cleanup-rules">
        {cleanup === null && error === null ? (
          <>
            <p className="screen-status loading">Looking for things to clean up…</p>
            <Skeleton rows={5} />
          </>
        ) : null}
        {cleanup !== null && groups.length === 0 ? (
          <section className="empty" aria-labelledby="cleanup-none">
            <h2 id="cleanup-none">Nothing to clean up</h2>
            <p className="empty-text">
              No cleanup rule matched anything in this scan.
            </p>
          </section>
        ) : null}

        {KIND_ORDER.map((kind) => {
          const cards = groups.filter((group) => group.kind === kind);
          if (cards.length === 0) {
            return null;
          }
          return (
            <section
              key={kind}
              className="cleanup-kind"
              aria-labelledby={`kind-${kind}`}
            >
              <h2 id={`kind-${kind}`} className="cleanup-kind-title">
                {KIND_LABELS[kind].title}
              </h2>
              <p className="cleanup-kind-hint">{KIND_LABELS[kind].hint}</p>
              {cards.map((group) => (
                <RuleCard
                  key={group.ruleId}
                  group={group}
                  selection={selection}
                  onToggleItem={(path) =>
                    setSelection((current) => toggleItem(current, path))
                  }
                  onToggleGroup={(card) =>
                    setSelection((current) => toggleGroup(current, card))
                  }
                />
              ))}
            </section>
          );
        })}

        {cleanup !== null &&
        (cleanup.refused > 0 || cleanup.problems.length > 0) ? (
          <details className="cleanup-notes">
            <summary>About these suggestions</summary>
            {cleanup.refused > 0 ? (
              <p>
                {formatCount(cleanup.refused)}{" "}
                {cleanup.refused === 1 ? "match was" : "matches were"} held back
                by the safety checks (system folders, app bundles, protected
                folders) and are not offered.
              </p>
            ) : null}
            {cleanup.problems.map((problem) => (
              <p key={problem}>Skipped rule: {problem}</p>
            ))}
          </details>
        ) : null}

        {groups.some((group) => group.deletable) ? (
          <footer className="cleanup-footer">
            <span className="cleanup-total" aria-live="polite">
              {total.items === 0
                ? "Nothing selected"
                : `${formatCount(total.items)} selected · ${formatBytes(total.bytes)}`}
            </span>
            <button
              type="button"
              className="button"
              disabled={total.items === 0 || busy}
              onClick={review}
            >
              Review and move to Trash…
            </button>
          </footer>
        ) : null}
      </div>

      <DuplicatesSection />

      {plan !== null ? (
        <Receipt plan={plan} onCancel={() => setPlan(null)} onDone={finished} />
      ) : null}
    </div>
  );
}

/** What a move did, with undo. Shared with the Apps screen. */
export function ResultPanel({
  result,
  undone,
  busy,
  onUndo,
  onDismiss,
}: {
  readonly result: DeletionResultDto;
  readonly undone: UndoResultDto | null;
  readonly busy: boolean;
  readonly onUndo: () => void;
  readonly onDismiss: () => void;
}) {
  const notMoved = result.items.filter((item) => item.status !== "trashed");
  const notBack =
    undone?.items.filter(
      (item) => item.result !== "restored" && item.result !== "not-trashed",
    ) ?? [];
  return (
    <section
      className="cleanup-result"
      aria-labelledby="cleanup-result-title"
      role="status"
    >
      <div className="cleanup-result-head">
        <h2 id="cleanup-result-title">
          {result.trashed === 0
            ? "Nothing was moved"
            : `Moved ${formatCount(result.trashed)} ${result.trashed === 1 ? "item" : "items"} (${formatBytes(result.trashedBytes)}) to the Trash`}
        </h2>
        <button
          type="button"
          className="dialog-close"
          aria-label="Dismiss"
          onClick={onDismiss}
        >
          ×
        </button>
      </div>
      <p className="cleanup-counts">
        {countBy(result.items, (item) => item.status, STATUS_ORDER)
          .map(
            ({ value, count }) =>
              `${STATUS_LABELS[value]}: ${formatCount(count)}`,
          )
          .join(" · ")}
      </p>
      {result.warning !== null ? (
        <p className="map-notice">{result.warning}</p>
      ) : null}
      {notMoved.length > 0 ? (
        <ul className="cleanup-reasons">
          {notMoved.map((item) => (
            <li key={item.path}>
              <span className="rule-path" title={item.path}>
                {item.path}
              </span>
              <span>
                {STATUS_LABELS[item.status]}
                {item.reason !== null ? `: ${item.reason}` : ""}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {IS_MAC && refusedForPermission(notMoved.map((item) => item.reason)) ? (
        <div className="banner" role="note">
          <p>
            <strong>macOS would not let StorageLens move these.</strong> Another app's
            protected data, such as an App Store app's folder in Library/Containers, can
            only be moved with Full Disk Access. Turn it on for StorageLens, then try again.
          </p>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => {
              api.openFullDiskAccessSettings().catch(() => undefined);
            }}
          >
            Open Full Disk Access settings
          </button>
        </div>
      ) : null}
      {undone === null ? (
        result.trashed > 0 ? (
          <button
            type="button"
            className="button button-secondary"
            disabled={busy}
            aria-busy={busy}
            onClick={onUndo}
          >
            Undo
          </button>
        ) : null
      ) : (
        <>
          <p className="cleanup-counts">
            {countBy(undone.items, (item) => item.result, UNDO_ORDER)
              .map(
                ({ value, count }) =>
                  `${UNDO_LABELS[value]}: ${formatCount(count)}`,
              )
              .join(" · ")}
          </p>
          {undone.warning !== null ? (
            <p className="map-notice">{undone.warning}</p>
          ) : null}
          {notBack.length > 0 ? (
            <ul className="cleanup-reasons">
              {notBack.map((item) => (
                <li key={item.path}>
                  <span className="rule-path" title={item.path}>
                    {item.path}
                  </span>
                  <span>
                    {UNDO_LABELS[item.result]}
                    {item.reason !== null ? `: ${item.reason}` : ""}
                  </span>
                </li>
              ))}
            </ul>
          ) : null}
        </>
      )}
    </section>
  );
}
