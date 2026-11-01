/**
 * Asks for macOS permissions at launch instead of mid-action (#149).
 *
 * Full Disk Access cannot be requested by an app, only turned on in System
 * Settings, so a banner says so while it is off, rechecking whenever the
 * window regains focus. Automation of Finder (used to trash App Store apps)
 * is asked for once now, with one harmless Apple Event. Administrator
 * passwords stay per action: macOS has no way to grant them ahead of time.
 */

import { useEffect, useState } from "react";

import { api, type DiskAccess } from "../lib/api";

export default function Permissions() {
  const [access, setAccess] = useState<DiskAccess>("unknown");
  const [hidden, setHidden] = useState(false);

  useEffect(() => {
    const check = () => {
      api.checkFullDiskAccess().then(setAccess).catch(() => undefined);
    };
    check();
    api.requestFinderAccess().catch(() => undefined);
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, []);

  if (access !== "denied" || hidden) {
    return null;
  }
  return (
    <div className="banner" role="note">
      <p>
        <strong>StorageLens needs Full Disk Access.</strong> Without it, protected folders
        are skipped in scans and other apps' data cannot be removed. Turn it on for
        StorageLens, then reopen the app.
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
      <button
        type="button"
        className="dialog-close"
        aria-label="Hide until next launch"
        onClick={() => setHidden(true)}
      >
        ×
      </button>
    </div>
  );
}
