/**
 * What an uninstalled app left behind (#120). Strong matches (the install
 * folder, folders named after the bundle id or publisher-and-app) start
 * ticked; name-only matches are listed apart and start unticked, because
 * another program could use a folder of the same name. Nothing moves until
 * the receipt is confirmed, and it goes to the Trash with undo.
 *
 * Registry keys (#121, Windows) cannot go to the Recycle Bin: each is
 * exported to a `.reg` backup first, and only deleted when that worked; the
 * backups can be imported again from here.
 */

import { useState } from "react";

import { api, type RegistryRemovalDto } from "../lib/api";

import type { AppRowDto, LeftoverDto } from "../lib/api";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
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
    () =>
      new Set(
        items
          .filter((item) => item.strong && item.canRemove)
          .map((item) => item.path),
      ),
  );
  const [keyResult, setKeyResult] = useState<RegistryRemovalDto | null>(null);
  const [restored, setRestored] = useState(false);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [keyBusy, setKeyBusy] = useState(false);
  const toggle = (path: string) =>
    setChosen((current) => {
      const next = new Set(current);
      if (!next.delete(path)) {
        next.add(path);
      }
      return next;
    });
  const files = items.filter((item) => item.kind !== "registry");
  const keys = items.filter((item) => item.kind === "registry");
  const strong = files.filter((item) => item.strong);
  const weak = files.filter((item) => !item.strong);
  const chosenFiles = files.filter((item) => chosen.has(item.path));
  const chosenKeys = keys.filter((item) => chosen.has(item.path));
  const bytes = chosenFiles.reduce((sum, item) => sum + item.bytes, 0);

  const list = (group: readonly LeftoverDto[]) => (
    <ul className="apps-satellites">
      {group.map((item) => (
        <li key={item.path}>
          <label
            title={
              item.canRemove
                ? undefined
                : "A key for all users: needs StorageLens to run as administrator"
            }
          >
            <input
              type="checkbox"
              checked={chosen.has(item.path)}
              disabled={
                !item.canRemove ||
                (item.kind === "registry" && keyResult !== null)
              }
              onChange={() => toggle(item.path)}
            />{" "}
            <span className="rule-path" title={item.path}>
              {item.path}
            </span>
          </label>
          <span>
            {item.kind === "registry"
              ? item.strong
                ? "registry key"
                : "registry key · name only"
              : `${item.kind === "install" ? "install folder · " : ""}~${formatBytes(item.bytes)}`}
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
      {keys.length > 0 ? (
        <>
          <p className="apps-detail-title">
            Registry keys left by {row.name}. Each is backed up to a .reg file
            before it is deleted.
          </p>
          {list(keys)}
          {keyResult === null ? (
            <div className="apps-remove-actions">
              <button
                type="button"
                className="button button-secondary"
                disabled={chosenKeys.length === 0 || keyBusy}
                onClick={() => {
                  setKeyBusy(true);
                  setKeyError(null);
                  api
                    .removeRegistryLeftovers(
                      row.id,
                      chosenKeys.map((key) => key.path),
                    )
                    .then(setKeyResult)
                    .catch((cause: unknown) => setKeyError(message(cause)))
                    .finally(() => setKeyBusy(false));
                }}
              >
                Back up and remove {formatCount(chosenKeys.length)}{" "}
                {chosenKeys.length === 1 ? "key" : "keys"}
              </button>
            </div>
          ) : (
            <div role="status">
              <p>
                {restored
                  ? "Registry keys restored."
                  : `${formatCount(keyResult.removed.length)} ${keyResult.removed.length === 1 ? "key" : "keys"} removed. Backups are in ${keyResult.backupDir}.`}
              </p>
              {keyResult.failed.map(([key, why]) => (
                <p key={key} className="map-notice">
                  {key}: {why}
                </p>
              ))}
              {keyResult.removed.length > 0 && !restored ? (
                <button
                  type="button"
                  className="button button-secondary"
                  disabled={keyBusy}
                  onClick={() => {
                    setKeyBusy(true);
                    setKeyError(null);
                    api
                      .restoreRegistryBackups(
                        keyResult.removed.map((key) => key.backup),
                      )
                      .then(() => setRestored(true))
                      .catch((cause: unknown) => setKeyError(message(cause)))
                      .finally(() => setKeyBusy(false));
                  }}
                >
                  Restore keys
                </button>
              ) : null}
            </div>
          )}
          {keyError !== null ? (
            <p className="map-notice" role="alert">
              {keyError}
            </p>
          ) : null}
        </>
      ) : null}
      {!canReview && files.length > 0 ? (
        <p className="empty-text">
          Moving leftovers to the Trash needs a finished scan. Start one from
          the Overview.
        </p>
      ) : null}
      <div className="apps-remove-actions">
        {files.length > 0 ? (
          <button
            type="button"
            className="button"
            disabled={chosenFiles.length === 0 || busy || !canReview}
            onClick={() => onReview(chosenFiles.map((item) => item.path))}
          >
            Review and move to Trash…
            {chosenFiles.length > 0
              ? ` (${formatCount(chosenFiles.length)} · ${formatBytes(bytes)})`
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
