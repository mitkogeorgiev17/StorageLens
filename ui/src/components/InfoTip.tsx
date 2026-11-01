/**
 * An ⓘ button that shows a short explanation in a bubble (#172), so long
 * help text stays out of the way until asked for. Click or Enter opens it;
 * a second click, Escape, or a click elsewhere closes it.
 */

import { useEffect, useId, useRef, useState, type ReactNode } from "react";

export default function InfoTip({
  label,
  children,
}: {
  /** What it explains, for screen readers: "About {label}". */
  readonly label: string;
  readonly children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const root = useRef<HTMLSpanElement | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
      }
    };
    const onPointer = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onPointer);
    };
  }, [open]);

  return (
    <span className="info-tip" ref={root}>
      <button
        type="button"
        className="info-button"
        aria-label={`About ${label}`}
        aria-expanded={open}
        aria-controls={id}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((shown) => !shown);
        }}
      >
        i
      </button>
      {open ? (
        <span id={id} role="note" className="info-bubble">
          {children}
        </span>
      ) : null}
    </span>
  );
}
