/**
 * Cleanup (#34): the screen that earns trust.
 *
 * Rule cards grouped by kind, each with its explanation and risk; a running
 * total of what is selected; a receipt listing every path before anything
 * moves; then, item by item, what moved, what was skipped and why — with
 * undo. Docker contents (#168) and duplicate files (#112) are sections at
 * the end; Docker removes through Docker itself, for good. Every move goes to the Trash through the safety crate's guard and
 * preflight re-stat; nothing here deletes anything itself.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import DockerSection from "../components/DockerSection";
import DuplicatesSection from "../components/DuplicatesSection";
import Receipt from "../components/Receipt";
import Skeleton from "../components/Skeleton";
import InfoTip from "../components/InfoTip";
import RuleCard from "../components/RuleCard";
import {
  api,
  type CleanupDto,
  type DeletionPlanDto,
  type DeletionResultDto,
  type RuleGroupDto,
  type ItemStatus,
  type UndoOutcome,
  type UndoResultDto,
} from "../lib/api";
import {
  STATUS_LABELS,
  UNDO_LABELS,
  cleanupSections,
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
  const { removable, manual, leftAlone } = cleanupSections(groups);
  const removableBytes = removable.reduce((sum, group) => sum + group.bytes, 0);
  const row = (group: RuleGroupDto) => (
    <RuleCard
      key={group.ruleId}
      group={group}
      selection={selection}
      onToggleItem={(path) => setSelection((current) => toggleItem(current, path))}
      onToggleGroup={(card) => setSelection((current) => toggleGroup(current, card))}
    />
  );
  const notes =
    cleanup !== null && (cleanup.refused > 0 || cleanup.problems.length > 0) ? (
      <InfoTip label="what was held back">
        {cleanup.refused > 0
          ? `${formatCount(cleanup.refused)} ${
              cleanup.refused === 1 ? "match was" : "matches were"
            } held back by the safety checks (system folders, app bundles, protected folders). `
          : null}
        {cleanup.problems.map((problem) => `Skipped rule: ${problem}. `)}
      </InfoTip>
    ) : null;

  return (
    <div className="cleanup-screen">
      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}

      {cleanup !== null && removable.length > 0 ? (
        <header className="cleanup-summary">
          <div className="cleanup-summary-figure">
            <span className="cleanup-summary-label">
              Can free {notes}
            </span>
            <span className="cleanup-summary-value">{formatBytes(removableBytes)}</span>
          </div>
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
        </header>
      ) : null}

      <div className="cleanup-rules">
        {cleanup === null && error === null ? (
          <>
            <p className="screen-status loading">Looking for things to clean up…</p>
            <Skeleton rows={5} />
          </>
        ) : null}
        {cleanup !== null && removable.length === 0 && manual.length === 0 ? (
          <section className="empty" aria-labelledby="cleanup-none">
            <h2 id="cleanup-none">Nothing to clean up</h2>
            <p className="empty-text">No cleanup rule found anything to free in this scan.</p>
          </section>
        ) : null}

        {removable.length > 0 ? (
          <section className="cleanup-section" aria-labelledby="cleanup-removable">
            <h2 id="cleanup-removable" className="cleanup-section-title">
              Can remove
              <InfoTip label="Can remove">
                These go to the Trash, with undo. Safe items start selected; anything
                marked Review is yours to check first.
              </InfoTip>
            </h2>
            <div className="rule-list">{removable.map(row)}</div>
          </section>
        ) : null}

        {manual.length > 0 ? (
          <section className="cleanup-section" aria-labelledby="cleanup-manual">
            <h2 id="cleanup-manual" className="cleanup-section-title">
              Free it yourself
              <InfoTip label="Free it yourself">
                StorageLens shows these but never removes them itself, such as the Trash
                (that is what makes its own moves undoable). Open a row for how.
              </InfoTip>
            </h2>
            <div className="rule-list">{manual.map(row)}</div>
          </section>
        ) : null}

        {leftAlone.length > 0 ? (
          <details className="cleanup-left-alone">
            <summary>
              Left alone · {formatCount(leftAlone.length)}{" "}
              {leftAlone.length === 1 ? "group" : "groups"}
            </summary>
            <p className="cleanup-section-hint">
              Your own data or the system's. Shown for the space it takes; StorageLens does
              not offer to remove it.
            </p>
            <div className="rule-list">{leftAlone.map(row)}</div>
          </details>
        ) : null}
      </div>

      <DockerSection />
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
