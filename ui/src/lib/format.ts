/**
 * The only place in the UI that turns numbers into text (AGENTS.md §8).
 *
 * Byte formatting matches `scanbench`'s `human_bytes` exactly, so the CLI and
 * the app never disagree about the same scan:
 *
 * - binary units (1 KB = 1024 B), labelled `KB`/`MB`/`GB`/`TB`/`PB` as Finder
 *   and Explorer users expect;
 * - two decimals below 100 of a unit, none from 100 up (`48.21 GB`, `212 MB`);
 * - a value that would round to 1024 of a unit is promoted (`1.00 GB`, never
 *   `1024 MB`);
 * - negative values (space freed, deltas) keep their sign: `-1.50 KB`.
 *
 * Callers pass `size_alloc` for "space used" (AGENTS.md invariant 1); this
 * module cannot tell the two sizes apart, so the label is the caller's job.
 */

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/** Shown for values that are not finite numbers. */
export const UNKNOWN = "—";

function decimalsFor(value: number): number {
  return value >= 100 ? 0 : 2;
}

function roundTo(value: number, decimals: number): number {
  const factor = 10 ** decimals;
  return Math.round(value * factor) / factor;
}

/** `1536` → `"1.50 KB"`. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) {
    return UNKNOWN;
  }
  if (bytes < 0) {
    return `-${formatBytes(-bytes)}`;
  }

  const whole = Math.floor(bytes);
  if (whole < 1024) {
    return `${whole} B`;
  }

  let value = whole;
  let unit = 0;
  while (value >= 1024 && unit + 1 < BYTE_UNITS.length) {
    value /= 1024;
    unit += 1;
  }

  // Round first: 1023.996 MB prints as `1024 MB`, which is wrong — promote it.
  if (roundTo(value, decimalsFor(value)) >= 1024 && unit + 1 < BYTE_UNITS.length) {
    value /= 1024;
    unit += 1;
  }

  return `${value.toFixed(decimalsFor(value))} ${BYTE_UNITS[unit]}`;
}

/** `184203` → `"184,203"`. Deterministic: does not depend on the system locale. */
export function formatCount(count: number): string {
  if (!Number.isFinite(count)) {
    return UNKNOWN;
  }
  const sign = count < 0 ? "-" : "";
  const digits = Math.floor(Math.abs(count)).toString();
  const groups: string[] = [];
  for (let end = digits.length; end > 0; end -= 3) {
    groups.unshift(digits.slice(Math.max(0, end - 3), end));
  }
  return sign + groups.join(",");
}

/**
 * Milliseconds as a short duration: `"850 ms"`, `"4.31 s"`, `"2 m 05 s"`,
 * `"1 h 02 m"`.
 */
export function formatDuration(milliseconds: number): string {
  if (!Number.isFinite(milliseconds)) {
    return UNKNOWN;
  }
  if (milliseconds < 0) {
    return `-${formatDuration(-milliseconds)}`;
  }
  if (milliseconds < 1000) {
    return `${Math.round(milliseconds)} ms`;
  }

  const seconds = milliseconds / 1000;
  if (seconds < 60) {
    return `${seconds.toFixed(2)} s`;
  }

  const totalSeconds = Math.floor(seconds);
  const pad = (value: number) => value.toString().padStart(2, "0");
  if (totalSeconds < 3600) {
    return `${Math.floor(totalSeconds / 60)} m ${pad(totalSeconds % 60)} s`;
  }
  const minutes = Math.floor(totalSeconds / 60);
  return `${Math.floor(minutes / 60)} h ${pad(minutes % 60)} m`;
}

/** `0.4231` → `"42.3%"`. */
export function formatPercent(fraction: number): string {
  if (!Number.isFinite(fraction)) {
    return UNKNOWN;
  }
  return `${(fraction * 100).toFixed(1)}%`;
}

const DATE_FORMAT = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
});

/**
 * Unix seconds → a short local date (`Mar 4, 2024` in en-US). `null`, 0, or a
 * non-finite time is unknown: platforms report 0 when they have no answer.
 */
export function formatDate(unixSeconds: number | null): string {
  if (unixSeconds === null || !Number.isFinite(unixSeconds) || unixSeconds === 0) {
    return UNKNOWN;
  }
  return DATE_FORMAT.format(new Date(unixSeconds * 1000));
}
