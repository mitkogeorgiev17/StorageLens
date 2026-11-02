/**
 * One cleanup rule as a compact row (#34, #172): checkbox, title, count,
 * risk, size. The explanation sits behind ⓘ; the matches open under the
 * row. Report-only rows carry the command to run, with Copy.
 */

import { useEffect, useRef, useState } from "react";

import type { RuleGroupDto } from "../lib/api";
import {
  KIND_LABELS,
  RISK_LABELS,
  groupState,
  visibleItems,
  type Selection,
} from "../lib/cleanup";
import { formatBytes, formatCount, shortPath } from "../lib/format";
import InfoTip from "./InfoTip";

/** Matches listed before "Show all". */
const PREVIEW_ITEMS = 8;

export default function RuleCard({
  group,
  selection,
  onToggleItem,
  onToggleGroup,
}: {
  readonly group: RuleGroupDto;
  readonly selection: Selection;
  readonly onToggleItem: (path: string) => void;
  readonly onToggleGroup: (group: RuleGroupDto) => void;
}) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState(false);
  const [copied, setCopied] = useState(false);
  const check = useRef<HTMLInputElement | null>(null);
  const state = groupState(selection, group);

  useEffect(() => {
    if (check.current !== null) {
      check.current.indeterminate = state === "some";
    }
  }, [state]);

  const items = visibleItems(group);
  const shown = all ? items : items.slice(0, PREVIEW_ITEMS);
  const titleId = `rule-${group.ruleId}`;
  const bodyId = `rule-body-${group.ruleId}`;

  const copy = () => {
    if (group.command === null) {
      return;
    }
    navigator.clipboard
      .writeText(group.command)
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
  };

  return (
    <article
      className="rule-row"
      aria-labelledby={titleId}
      data-open={open}
      data-deletable={group.deletable}
    >
      <div className="rule-row-head">
        {group.deletable ? (
          <input
            ref={check}
            type="checkbox"
            className="rule-check"
            checked={state === "all"}
            onChange={() => onToggleGroup(group)}
            aria-label={`Select every item of ${group.title}`}
          />
        ) : (
          <span className="rule-check" aria-hidden="true" />
        )}
        <button
          type="button"
          className="rule-toggle"
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={() => setOpen((shown) => !shown)}
        >
          <span className="rule-chevron" aria-hidden="true" />
          <span id={titleId} className="rule-title">
            {group.title}
          </span>
          <span className="rule-count">
            {formatCount(items.length)} {items.length === 1 ? "item" : "items"}
          </span>
        </button>
        <InfoTip label={group.title}>
          <strong>{KIND_LABELS[group.kind].title}.</strong> {group.explain}
        </InfoTip>
        <span className="risk-badge" data-risk={group.risk}>
          {RISK_LABELS[group.risk]}
        </span>
        <span className="rule-bytes">
          {group.bytes > 0 ? formatBytes(group.bytes) : "—"}
        </span>
      </div>
      {open ? (
        <div id={bodyId} className="rule-row-body">
          {group.command !== null ? (
            <p className="rule-command">
              <code>{group.command}</code>
              <button
                type="button"
                className="button button-secondary"
                onClick={copy}
              >
                {copied ? "Copied" : "Copy"}
              </button>
            </p>
          ) : null}
          <ul className="rule-items">
            {shown.map((item) => (
              <li key={item.path} className="rule-item">
                {group.deletable ? (
                  <label className="rule-item-label">
                    <input
                      type="checkbox"
                      checked={selection.has(item.path)}
                      onChange={() => onToggleItem(item.path)}
                    />
                    <span className="rule-path" title={item.path}>
                      {shortPath(item.path)}
                    </span>
                  </label>
                ) : (
                  <span className="rule-path" title={item.path}>
                    {shortPath(item.path)}
                  </span>
                )}
                <span className="rule-item-bytes">
                  {item.bytes > 0 ? formatBytes(item.bytes) : ""}
                </span>
              </li>
            ))}
          </ul>
          {items.length > PREVIEW_ITEMS ? (
            <button
              type="button"
              className="link-button"
              onClick={() => setAll((shown) => !shown)}
            >
              {all ? "Show fewer" : `Show all ${formatCount(items.length)}`}
            </button>
          ) : null}
        </div>
      ) : null}
    </article>
  );
}
