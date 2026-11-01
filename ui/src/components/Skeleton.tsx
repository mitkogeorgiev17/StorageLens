/**
 * Shimmering placeholder rows while a slow list loads (#160): the app list,
 * startup items, cleanup suggestions. Purely visual; the status line next to
 * it says what is happening.
 */

export default function Skeleton({ rows = 6 }: { readonly rows?: number }) {
  return (
    <div className="skeleton" aria-hidden="true">
      {Array.from({ length: rows }, (_, index) => (
        <div key={index} className="skeleton-row">
          <span className="skeleton-bar skeleton-icon" />
          <span className="skeleton-bar" style={{ width: `${40 + ((index * 37) % 45)}%` }} />
          <span className="skeleton-bar skeleton-end" />
        </div>
      ))}
    </div>
  );
}
