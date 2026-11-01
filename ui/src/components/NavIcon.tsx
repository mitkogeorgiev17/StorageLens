/**
 * Sidebar icons: one inline 24 × 24 stroke drawing per screen. Inline SVG, no
 * icon dependency (AGENTS.md §2.3); `currentColor`, so they follow the nav
 * item's text colour in both themes. Decorative: the label is the name.
 */

import type { RouteId } from "../store";

const PATHS: Readonly<Record<RouteId, readonly string[]>> = {
  // Four tiles of a dashboard.
  overview: ["M4 4h7v9H4z", "M13 4h7v5h-7z", "M13 11h7v9h-7z", "M4 15h7v5H4z"],
  // A treemap: one big tile, the rest split.
  map: ["M3 4h18v16H3z", "M12 4v16", "M12 12h9", "M16.5 12v8"],
  // A file with an upward arrow: biggest first.
  "large-files": ["M14 3H6v18h12V7z", "M14 3v4h4", "M12 17v-7", "M9 13l3-3 3 3"],
  // Stacked layers: saved views.
  // An app grid.
  apps: ["M4 4h6v6H4z", "M14 4h6v6h-6z", "M4 14h6v6H4z", "M14 14h6v6h-6z"],
  // A sparkle: safe to clean.
  cleanup: ["M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z", "M19 16l.7 2.3L22 19l-2.3.7L19 22l-.7-2.3L16 19l2.3-.7z"],
  // Two overlapping copies.
  // A clock turning back.
  history: ["M3 12a9 9 0 1 0 3-6.7", "M3 4v5h5", "M12 7v5l3 2"],
};

export default function NavIcon({ id }: { readonly id: RouteId }) {
  return (
    <svg
      className="nav-icon"
      viewBox="0 0 24 24"
      width={18}
      height={18}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[id].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}
