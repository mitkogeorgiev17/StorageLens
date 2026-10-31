/**
 * The folder path from the scan root to the folder the map shows. Every
 * ancestor is a button; the current folder is plain text.
 */

import type { Crumb } from "../lib/mapTree";

export interface BreadcrumbProps {
  readonly crumbs: readonly Crumb[];
  /** Go to `crumbs[index]`. */
  readonly onNavigate: (index: number) => void;
}

export default function Breadcrumb({ crumbs, onNavigate }: BreadcrumbProps) {
  return (
    <nav className="breadcrumb" aria-label="Folder path">
      <ol>
        {crumbs.map((crumb, index) => {
          const last = index === crumbs.length - 1;
          return (
            <li key={crumb.id}>
              {last ? (
                <span className="breadcrumb-current" aria-current="location">
                  {crumb.name}
                </span>
              ) : (
                <button type="button" className="breadcrumb-link" onClick={() => onNavigate(index)}>
                  {crumb.name}
                </button>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
