/**
 * The Map's "group by" control (#26): one click switches from where space is
 * (Folder) to what it is (Type, App, Age, Size, Cleanliness).
 *
 * Native radio buttons drawn as a segmented control, so the arrow keys move
 * between groupings with no extra code.
 */

import type { Grouping } from "../lib/api";
import { GROUPINGS } from "../lib/mapView";

/** Groupings that cannot show anything useful yet, and why. Cleanliness
 * became available with the cleanup rules (#31). */
const UNAVAILABLE: Partial<Record<Grouping, string>> = {};

export default function GroupingSelector({
  value,
  onChange,
}: {
  readonly value: Grouping;
  readonly onChange: (by: Grouping) => void;
}) {
  return (
    <fieldset className="segmented">
      <legend className="visually-hidden">Group by</legend>
      {GROUPINGS.map(({ id, label }) => {
        const unavailable = UNAVAILABLE[id];
        return (
          <label
            key={id}
            className="segment"
            title={unavailable}
            data-checked={value === id}
            data-disabled={unavailable !== undefined}
          >
            <input
              type="radio"
              name="map-grouping"
              value={id}
              checked={value === id}
              disabled={unavailable !== undefined}
              onChange={() => onChange(id)}
            />
            {label}
            {unavailable === undefined ? null : (
              <span className="visually-hidden">({unavailable})</span>
            )}
          </label>
        );
      })}
    </fieldset>
  );
}
