/**
 * Startup (#114): what starts at login, and a switch for each.
 *
 * Switching keeps the entry and only stops it starting at login, the way
 * Task Manager (Windows) and launchd's own overrides (macOS) do, so it can be
 * switched back. Entries this app cannot switch (another user's or the
 * system's) point to the system's own settings instead.
 */

import { useCallback, useEffect, useState } from "react";

import { api, type StartupItemDto } from "../lib/api";
import { formatCount } from "../lib/format";

const IS_MAC =
  typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);
const SETTINGS = IS_MAC ? "Login Items settings" : "Startup apps settings";

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function StartupScreen() {
  const [items, setItems] = useState<readonly StartupItemDto[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [switching, setSwitching] = useState<string | null>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    return api
      .listStartup()
      .then(setItems)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = (item: StartupItemDto) => {
    setSwitching(item.id);
    setError(null);
    api
      .setStartup(item.id, !item.enabled)
      .then(setItems)
      .catch((cause: unknown) => setError(message(cause)))
      .finally(() => setSwitching(null));
  };

  const enabled = items?.filter((item) => item.enabled).length ?? 0;
  const locked = items?.some((item) => !item.canToggle) ?? false;

  return (
    <div className="apps-screen">
      <div className="apps-toolbar">
        <span className="screen-status" aria-live="polite">
          {loading
            ? "Reading startup items…"
            : items !== null
              ? `${formatCount(items.length)} items · ${formatCount(enabled)} start at login`
              : ""}
        </span>
        <span className="startup-actions">
          <button
            type="button"
            className="button button-secondary"
            onClick={() =>
              void api
                .openStartupSettings()
                .catch((cause: unknown) => setError(message(cause)))
            }
          >
            Open {SETTINGS}
          </button>
          <button
            type="button"
            className="button button-secondary"
            disabled={loading}
            onClick={() => void load()}
          >
            Refresh
          </button>
        </span>
      </div>

      {error !== null ? (
        <p className="map-notice" role="alert">
          {error}
        </p>
      ) : null}

      {items !== null && items.length === 0 ? (
        <section className="empty" aria-labelledby="startup-none">
          <h2 id="startup-none">Nothing starts at login</h2>
          <p className="empty-text">
            {IS_MAC
              ? `No launch agents or daemons start at login. Apps added under Login Items are shown in ${SETTINGS}.`
              : "No Run entries or Startup-folder shortcuts were found."}
          </p>
        </section>
      ) : null}

      {items !== null && items.length > 0 ? (
        <table className="apps-table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Registered in</th>
              <th scope="col">Starts at login</th>
            </tr>
          </thead>
          <tbody>
            {items.map((item) => (
              <tr key={item.id}>
                <td>
                  <span className="apps-name">{item.name}</span>
                  <span
                    className="startup-command rule-path"
                    title={item.command}
                  >
                    {item.command}
                  </span>
                </td>
                <td>{item.source}</td>
                <td>
                  <label
                    className="startup-switch"
                    title={
                      item.canToggle
                        ? undefined
                        : IS_MAC
                          ? `Needs administrator rights. Use ${SETTINGS}.`
                          : "Needs StorageLens to run as administrator."
                    }
                  >
                    <input
                      type="checkbox"
                      checked={item.enabled}
                      disabled={!item.canToggle || switching !== null}
                      onChange={() => toggle(item)}
                    />{" "}
                    {switching === item.id
                      ? "Switching…"
                      : item.enabled
                        ? "On"
                        : "Off"}
                  </label>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}

      {locked ? (
        <p className="empty-text">
          Greyed-out switches belong to every user or the system.{" "}
          {IS_MAC
            ? `Switch them in ${SETTINGS}.`
            : "Run StorageLens as administrator to switch them."}
          {IS_MAC
            ? ` Apps added under Login Items there are not listed here.`
            : " Startup tasks in Task Scheduler are not listed."}
        </p>
      ) : null}
    </div>
  );
}
