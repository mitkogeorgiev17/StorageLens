/**
 * The Overview screen (PLAN §13): "how bad is it?" in one glance.
 *
 * - The reclaim headline, split into safe and needs-review (PLAN §10), from
 *   the rules engine's own score (`get_rules`): confidence-weighted, each
 *   byte once, report-only matches excluded. If it fails, the headline says
 *   so instead of guessing.
 * - The capacity gauge: this scan, other used space, purgeable, free.
 * - The category bar, the ten biggest items at the scan root, and the biggest
 *   cleanup wins.
 *
 * Every bar's segments add up to its stated total (see `lib/overview.ts`), and
 * every number goes through `lib/format.ts`. Keyed by scan id, so a rescan
 * reloads everything. Before a scan (or after a failed one) the screen is the
 * scan picker; during one, its progress.
 */

import { useEffect, useState } from "react";

import { api, type OverviewDto, type CleanupDto } from "../lib/api";
import { formatBytes, formatCount, formatPercent } from "../lib/format";
import {
  capacitySegments,
  categorySegments,
  reclaimSummary,
  topWins,
  type Segment,
} from "../lib/overview";
import { CATEGORY_COLORS } from "../lib/treemap";
import ScanPicker from "../components/ScanPicker";
import ScanProgress from "../components/ScanProgress";
import { useAppStore, type ScanSummary } from "../store";

const IS_MAC =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

type Rules =
  | { readonly kind: "loading" }
  | { readonly kind: "ready"; readonly rules: CleanupDto }
  | { readonly kind: "unavailable"; readonly reason: string };

export default function OverviewScreen() {
  const scan = useAppStore((state) => state.scan);
  switch (scan.kind) {
    case "scanning":
      return (
        <div className="overview">
          <ScanProgress scan={scan} />
        </div>
      );
    case "idle":
    case "failed":
      return (
        <div className="overview">
          <section className="card" aria-labelledby="start-title">
            <h2 id="start-title" className="card-title">
              {scan.kind === "idle" ? "No scan yet" : "The scan failed"}
            </h2>
            {scan.kind === "failed" ? (
              <p className="picker-error" role="alert">
                {scan.root === ""
                  ? scan.message
                  : `${scan.root}: ${scan.message}`}
              </p>
            ) : (
              <p className="card-note card-lead">
                Scan a volume or folder to see where the space went and how much
                can safely be freed. Nothing leaves this machine.
              </p>
            )}
            <ScanPicker />
          </section>
        </div>
      );
    case "done":
      return <OverviewView key={scan.summary.scanId} summary={scan.summary} />;
  }
}

function OverviewView({ summary }: { readonly summary: ScanSummary }) {
  const { scanId } = summary;
  const [overview, setOverview] = useState<OverviewDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rules, setRules] = useState<Rules>({ kind: "loading" });

  useEffect(() => {
    let live = true;
    api
      .getOverview(scanId)
      .then((loaded) => live && setOverview(loaded))
      .catch((cause: unknown) => live && setError(message(cause)));
    api
      .getRules(scanId)
      .then((loaded) => live && setRules({ kind: "ready", rules: loaded }))
      .catch(
        (cause: unknown) =>
          live && setRules({ kind: "unavailable", reason: message(cause) }),
      );
    return () => {
      live = false;
    };
  }, [scanId]);

  if (error !== null) {
    return (
      <section className="empty" role="alert">
        <h2>The overview could not be loaded</h2>
        <p className="empty-text">{error}</p>
      </section>
    );
  }
  if (overview === null) {
    return <p className="loading">Loading…</p>;
  }

  return (
    <div className="overview">
      {summary.status !== "complete" ? (
        <p className="banner" role="note">
          {summary.status === "cancelled"
            ? "This scan was cancelled. The totals below cover only what it reached."
            : "Some folders could not be read. The totals below are partial."}
        </p>
      ) : null}
      <Reclaim rules={rules} />
      <Capacity overview={overview} />
      <Categories overview={overview} />
      <div className="overview-columns">
        <Biggest overview={overview} />
        <Wins rules={rules} />
      </div>
    </div>
  );
}

// --- Cards ---------------------------------------------------------------------------

function Reclaim({ rules }: { readonly rules: Rules }) {
  if (rules.kind !== "ready") {
    return (
      <section className="card reclaim" aria-labelledby="reclaim-title">
        <h2 id="reclaim-title" className="reclaim-headline">
          {rules.kind === "loading"
            ? "Estimating what can be freed…"
            : "Reclaimable space is not estimated yet"}
        </h2>
        <dl className="reclaim-split">
          <div>
            <dt>Safe</dt>
            <dd>—</dd>
          </div>
          <div>
            <dt>Needs review</dt>
            <dd>—</dd>
          </div>
        </dl>
        {rules.kind === "unavailable" ? (
          <p className="card-note">
            Cleanup estimates come with the cleanup rules ({rules.reason}).
          </p>
        ) : null}
      </section>
    );
  }
  const reclaim = reclaimSummary(rules.rules);
  return (
    <section className="card reclaim" aria-labelledby="reclaim-title">
      <h2 id="reclaim-title" className="reclaim-headline">
        {reclaim.safe > 0
          ? `You can safely free ~${formatBytes(reclaim.safe)}`
          : "Nothing is safe to remove automatically"}
      </h2>
      <dl className="reclaim-split">
        <div>
          <dt>Safe</dt>
          <dd>{formatBytes(reclaim.safe)}</dd>
        </div>
        <div>
          <dt>Needs review</dt>
          <dd>{formatBytes(reclaim.review)}</dd>
        </div>
        {reclaim.manual > 0 ? (
          <div>
            <dt>Free it yourself</dt>
            <dd>{formatBytes(reclaim.manual)}</dd>
          </div>
        ) : null}
      </dl>
      {reclaim.manual > 0 ? (
        <p className="card-note">
          {reclaim.manualTitles.join(", ")}: StorageLens shows how to free
          these on the Cleanup screen, but does not remove them itself.
        </p>
      ) : null}
    </section>
  );
}

/** A stacked bar plus its legend. The legend carries the numbers; colour only groups. */
function StackedBar({
  segments,
  total,
  label,
  colorOf,
  detail,
}: {
  readonly segments: readonly Segment[];
  readonly total: number;
  readonly label: string;
  readonly colorOf: (segment: Segment) => string;
  readonly detail?: (segment: Segment) => string | null;
}) {
  const visible = segments.filter((segment) => segment.bytes > 0);
  return (
    <>
      <div
        className="bar"
        role="img"
        aria-label={`${label}: ${visible
          .map((segment) => `${segment.label} ${formatBytes(segment.bytes)}`)
          .join(", ")} of ${formatBytes(total)}`}
      >
        {visible.map((segment) => (
          <span
            key={segment.key}
            className="bar-segment"
            style={{ flexGrow: segment.fraction, background: colorOf(segment) }}
          />
        ))}
      </div>
      <ul className="legend">
        {visible.map((segment) => {
          const extra = detail?.(segment) ?? null;
          return (
            <li key={segment.key}>
              <span
                className="legend-swatch"
                style={{ background: colorOf(segment) }}
              />
              <span className="legend-label">{segment.label}</span>
              <span className="legend-value">{formatBytes(segment.bytes)}</span>
              <span className="legend-share">
                {formatPercent(segment.fraction)}
              </span>
              {extra === null ? null : (
                <span className="legend-extra">{extra}</span>
              )}
            </li>
          );
        })}
      </ul>
    </>
  );
}

const GAUGE_COLORS: Readonly<Record<string, string>> = {
  scan: "var(--gauge-scan)",
  other: "var(--gauge-other)",
  purgeable: "var(--gauge-purgeable)",
  free: "var(--gauge-free)",
};

function Capacity({ overview }: { readonly overview: OverviewDto }) {
  const { volume } = overview;
  if (volume === null) {
    return (
      <section className="card" aria-labelledby="capacity-title">
        <h2 id="capacity-title" className="card-title">
          Capacity
        </h2>
        <p className="card-note">
          The capacity of the volume holding{" "}
          {overview.scan.root ?? overview.rootName} is unavailable. This scan
          found {formatBytes(overview.scan.bytesAlloc)}.
        </p>
      </section>
    );
  }
  const gauge = capacitySegments(volume, overview.scan.bytesAlloc);
  const notes: string[] = [];
  if (gauge.clamped) {
    notes.push(
      "This scan counts more than the volume reports in use (shared blocks such as clones and snapshots), so its share is capped at the used space.",
    );
  }
  if (IS_MAC && volume.purgeable === 0) {
    notes.push("Purgeable space is not measured yet; it is counted as free.");
  }
  return (
    <section className="card" aria-labelledby="capacity-title">
      <div className="card-heading">
        <h2 id="capacity-title" className="card-title">
          {volume.name}
        </h2>
        <span className="card-meta">
          {formatBytes(volume.used)} used of {formatBytes(volume.total)} ·{" "}
          {formatBytes(volume.available)} available
        </span>
      </div>
      <StackedBar
        segments={gauge.segments}
        total={gauge.total}
        label="Capacity"
        colorOf={(segment) => GAUGE_COLORS[segment.key] ?? "var(--gauge-other)"}
      />
      {notes.map((note) => (
        <p key={note} className="card-note">
          {note}
        </p>
      ))}
    </section>
  );
}

function Categories({ overview }: { readonly overview: OverviewDto }) {
  const total = overview.scan.bytesAlloc;
  const segments = categorySegments(overview.categories, total);
  const files = new Map(
    overview.categories.map((category) => [
      category.category,
      category.fileCount,
    ]),
  );
  return (
    <section className="card" aria-labelledby="categories-title">
      <div className="card-heading">
        <h2 id="categories-title" className="card-title">
          By type
        </h2>
        <span className="card-meta">
          {formatBytes(total)} in {formatCount(overview.scan.files)} files
        </span>
      </div>
      <StackedBar
        segments={segments}
        total={total}
        label="Space by type"
        colorOf={(segment) =>
          segment.key in CATEGORY_COLORS
            ? CATEGORY_COLORS[segment.key as keyof typeof CATEGORY_COLORS]
            : "var(--gauge-other)"
        }
        detail={(segment) => {
          const count = files.get(segment.key as keyof typeof CATEGORY_COLORS);
          return count === undefined ? null : `${formatCount(count)} files`;
        }}
      />
    </section>
  );
}

function Biggest({ overview }: { readonly overview: OverviewDto }) {
  const total = overview.scan.bytesAlloc;
  const largest = overview.biggest[0]?.sizeAlloc ?? 0;
  return (
    <section className="card" aria-labelledby="biggest-title">
      <h2 id="biggest-title" className="card-title">
        Biggest in {overview.rootName}
      </h2>
      {overview.biggest.length === 0 ? (
        <p className="card-note">Nothing was found here.</p>
      ) : (
        <ol className="ranked">
          {overview.biggest.map((item) => (
            <li key={item.id}>
              <span className="ranked-name" title={item.name}>
                {item.name}
                {item.kind === "Dir" ? (
                  <span className="ranked-kind"> · folder</span>
                ) : null}
              </span>
              <span className="ranked-value">
                {formatBytes(item.sizeAlloc)}
              </span>
              <span className="ranked-share">
                {formatPercent(total > 0 ? item.sizeAlloc / total : 0)}
              </span>
              <span
                className="ranked-bar"
                aria-hidden="true"
                style={{
                  width: `${largest > 0 ? (item.sizeAlloc / largest) * 100 : 0}%`,
                }}
              />
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

function Wins({ rules }: { readonly rules: Rules }) {
  const wins = rules.kind === "ready" ? topWins(rules.rules.groups) : [];
  return (
    <section className="card" aria-labelledby="wins-title">
      <h2 id="wins-title" className="card-title">
        Top cleanup wins
      </h2>
      {rules.kind === "loading" ? <p className="card-note loading">Loading…</p> : null}
      {rules.kind === "unavailable" ? (
        <p className="card-note">
          Cleanup recommendations arrive with the cleanup rules.
        </p>
      ) : null}
      {rules.kind === "ready" && wins.length === 0 ? (
        <p className="card-note">Nothing to clean up. Nice.</p>
      ) : null}
      {wins.length > 0 ? (
        <ol className="ranked">
          {wins.map((win) => (
            <li key={win.ruleId}>
              <span className="ranked-name">{win.title}</span>
              <span className="ranked-value">{formatBytes(win.bytes)}</span>
              <span className="risk" data-risk={win.risk}>
                {win.risk === "safe" ? "Safe" : "Review"}
              </span>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}
