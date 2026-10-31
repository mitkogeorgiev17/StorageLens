/**
 * One cleanup rule's card (#34): what it found, why it is safe (or not) to
 * remove, and — for deletable rules — a checkbox per match plus one for the
 * whole card. Report-only cards show the command to run instead.
 */

import { useEffect, useRef, useState } from "react";

import type { RuleGroupDto } from "../lib/api";
import { RISK_LABELS, groupState, type Selection } from "../lib/cleanup";
import { formatBytes, formatCount } from "../lib/format";

/** Matches listed before "Show all". */
const PREVIEW_ITEMS = 5;

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
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const all = useRef<HTMLInputElement | null>(null);
  const state = groupState(selection, group);

  useEffect(() => {
    if (all.current !== null) {
      all.current.indeterminate = state === "some";
    }
  }, [state]);

  const shown = expanded ? group.items : group.items.slice(0, PREVIEW_ITEMS);
  const titleId = `rule-${group.ruleId}`;

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
      className="rule-card"
      aria-labelledby={titleId}
      data-deletable={group.deletable}
    >
      <header className="rule-card-header">
        {group.deletable ? (
          <input
            ref={all}
            type="checkbox"
            className="rule-check"
            checked={state === "all"}
            onChange={() => onToggleGroup(group)}
            aria-label={`Select every item of ${group.title}`}
          />
        ) : null}
        <h3 id={titleId} className="rule-title">
          {group.title}
        </h3>
        <span className="risk-badge" data-risk={group.risk}>
          {group.deletable ? RISK_LABELS[group.risk] : "Report only"}
        </span>
        <span className="rule-bytes">
          {group.bytes > 0 ? formatBytes(group.bytes) : ""}
        </span>
      </header>
      <p className="rule-explain">{group.explain}</p>
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
                  {item.path}
                </span>
              </label>
            ) : (
              <span className="rule-path" title={item.path}>
                {item.path}
              </span>
            )}
            <span className="rule-item-bytes">
              {item.bytes > 0 ? formatBytes(item.bytes) : ""}
            </span>
          </li>
        ))}
      </ul>
      {group.items.length > PREVIEW_ITEMS ? (
        <button
          type="button"
          className="link-button"
          aria-expanded={expanded}
          onClick={() => setExpanded((open) => !open)}
        >
          {expanded
            ? "Show fewer"
            : `Show all ${formatCount(group.items.length)}`}
        </button>
      ) : null}
    </article>
  );
}
