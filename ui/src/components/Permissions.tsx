/**
 * Every macOS permission, asked for at launch (#150, #155), so nothing is
 * asked in the middle of a scan or a removal.
 *
 * Shown while anything is missing, and it cannot be skipped (#175): nothing
 * scans until every permission is on. Rechecked whenever the window regains
 * focus (the user comes back from System Settings). Full Disk Access and
 * App Management cannot be requested by an app, only switched on in System
 * Settings; Finder automation is asked for with one harmless Apple Event.
 * Administrator passwords stay per action: macOS cannot grant them ahead of
 * time.
 */

import { useCallback, useEffect, useState } from "react";

import { api, type DiskAccess } from "../lib/api";
import { useAppStore } from "../store";

const IS_MAC = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

type Pane = "AllFiles" | "AppBundles" | "Automation";

interface Item {
  readonly name: string;
  readonly why: string;
  readonly ok: boolean | null;
  readonly pane: Pane;
}

export default function Permissions() {
  const [disk, setDisk] = useState<DiskAccess>("unknown");
  const [apps, setApps] = useState<boolean | null>(null);
  const [finder, setFinder] = useState<boolean | null>(null);
  const [checked, setChecked] = useState(false);
  const permitted = useAppStore((state) => state.permitted);
  const granted = useAppStore((state) => state.permissionsGranted);

  const check = useCallback(() => {
    // A check that fails leaves its state unknown, which does not block.
    void Promise.all([
      api.checkFullDiskAccess().then(setDisk).catch(() => undefined),
      api.checkAppManagement().then(setApps).catch(() => undefined),
      api.requestFinderAccess().then(setFinder).catch(() => undefined),
    ]).then(() => setChecked(true));
  }, []);

  useEffect(() => {
    if (!IS_MAC) {
      return undefined;
    }
    check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, [check]);

  const items: Item[] = [
    {
      name: "Full Disk Access",
      why: "Scan every folder, and remove apps' data and containers.",
      ok: disk === "granted" ? true : disk === "denied" ? false : null,
      pane: "AllFiles",
    },
    {
      name: "App Management",
      why: "Move apps in Applications to the Trash.",
      ok: apps,
      pane: "AppBundles",
    },
    {
      name: "Automation: Finder",
      why: "Move App Store apps, which only Finder may move, to the Trash.",
      ok: finder,
      pane: "Automation",
    },
  ];
  const missing = items.some((item) => item.ok === false);

  useEffect(() => {
    if (IS_MAC && checked && !missing && !permitted) {
      granted();
    }
  }, [checked, missing, permitted, granted]);

  // Shown whenever something is off, even after the launch scan started.
  if (!IS_MAC || !missing) {
    return null;
  }
  return (
    <div className="dialog-backdrop">
      <section className="dialog permissions" role="dialog" aria-modal="true" aria-labelledby="permissions-title">
        <h2 id="permissions-title">StorageLens needs a few permissions</h2>
        <p className="empty-text">
          Turn these on now so nothing is asked in the middle of a scan or a removal. macOS
          only lets you switch them on in System Settings; StorageLens checks again when you
          come back. If Full Disk Access stays Off after you turn it on, reopen StorageLens.
        </p>
        <ul className="permissions-list">
          {items.map((item) => (
            <li key={item.name}>
              <span className="permissions-state" data-ok={String(item.ok)}>
                {item.ok === true ? "On" : item.ok === false ? "Off" : "Unknown"}
              </span>
              <span className="permissions-text">
                <strong>{item.name}</strong>
                <span>{item.why}</span>
              </span>
              {item.ok === true ? null : (
                <button
                  type="button"
                  className="button button-secondary"
                  onClick={() => {
                    api.openPrivacyPane(item.pane).catch(() => undefined);
                  }}
                >
                  Open settings
                </button>
              )}
            </li>
          ))}
        </ul>
        <p className="empty-text">
          Your administrator password is still asked when removing system helpers or App
          Store apps: macOS does not allow granting it ahead of time.
        </p>
        <div className="apps-remove-actions">
          <span className="screen-status loading">Waiting for every permission to be on…</span>
          {disk === "denied" ? (
            <button
              type="button"
              className="button button-secondary"
              onClick={() => {
                api.relaunch().catch(() => undefined);
              }}
            >
              Reopen StorageLens
            </button>
          ) : null}
        </div>
      </section>
    </div>
  );
}
