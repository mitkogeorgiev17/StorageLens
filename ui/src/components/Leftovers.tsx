/**
 * What an uninstalled app left behind (#120). Strong matches (the install
 * folder, folders named after the bundle id or publisher-and-app) start
 * ticked; name-only matches are listed apart and start unticked, because
 * another program could use a folder of the same name. Nothing moves until
 * the receipt is confirmed, and it goes to the Trash with undo.
 */

import { useState } from "react";

import type { AppRowDto, LeftoverDto } from "../lib/api";
import { formatBytes, formatCount } from "../lib/format";

export default function Leftovers({
  row,
  items,
  busy,
  canReview,
  onReview,
  onDone,
}: {
  readonly row: AppRowDto;
  readonly items: readonly LeftoverDto[];
  readonly busy: boolean;
  /** A finished scan exists; receipts need one. */
  readonly canReview: boolean;
  readonly onReview: (paths: readonly string[]) => void;
  readonly onDone: () => void;
}) {
  const [chosen, setChosen] = useState<ReadonlySet<string>>(
    () => new Set(items.filter((item) => item.strong).map((item) => item.path)),
  );
  const toggle = (path: string) =>
    setChosen((current) => {
      const next = new Set(current);
      if (!next.delete(path)) {
        next.add(path);
      }
      return next;
    });
  const strong = items.filter((item) => item.strong);
  const weak = items.filter((item) => !item.strong);
  const bytes = items
    .filter((item) => chosen.has(item.path))
    .reduce((sum, item) => sum + item.bytes, 0);

  const list = (group: readonly LeftoverDto[]) => (
    <ul className="apps-satellites">
      {group.map((item) => (
        <li key={item.path}>
          <label>
            <input
              type="checkbox"
              checked={chosen.has(item.path)}
              onChange={() => toggle(item.path)}
            />{" "}
            <span className="rule-path" title={item.path}>
              {item.path}
            </span>
          </label>
          <span>
            {item.kind === "install" ? "install folder · " : ""}~
            {formatBytes(item.bytes)}
          </span>
        </li>
      ))}
    </ul>
  );

  return (
    <section className="apps-remove" aria-labelledby="leftovers-title">
      <h2 id="leftovers-title">{row.name} was uninstalled</h2>
      {items.length === 0 ? (
        <p>Nothing was left behind.</p>
      ) : (
        <>
          {strong.length > 0 ? (
            <>
              <p className="apps-detail-title">
                Left behind by {row.name}: its install folder or data named
                after it.
              </p>
              {list(strong)}
            </>
          ) : null}
          {weak.length > 0 ? (
            <>
              <p className="apps-detail-title">
                Possibly {row.name}'s, matched by name only. Another program
                could use these; check before ticking.
              </p>
              {list(weak)}
            </>
          ) : null}
        </>
      )}
      {!canReview && items.length > 0 ? (
        <p className="empty-text">
          Moving leftovers to the Trash needs a finished scan. Start one from
          the Overview.
        </p>
      ) : null}
      <div className="apps-remove-actions">
        {items.length > 0 ? (
          <button
            type="button"
            className="button"
            disabled={chosen.size === 0 || busy || !canReview}
            onClick={() => onReview([...chosen])}
          >
            Review and move to Trash…
            {chosen.size > 0
              ? ` (${formatCount(chosen.size)} · ${formatBytes(bytes)})`
              : ""}
          </button>
        ) : null}
        <button
          type="button"
          className="button button-secondary"
          onClick={onDone}
        >
          {items.length === 0 ? "Done" : "Keep them"}
        </button>
      </div>
    </section>
  );
}
