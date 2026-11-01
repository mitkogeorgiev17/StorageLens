/**
 * The Docker section's selection arithmetic (#168). An item is keyed by kind
 * and reference; items in use can never be selected.
 */

import type { DockerItemDto, DockerKind } from "./api";

export type DockerSelection = ReadonlySet<string>;

export const DOCKER_KINDS: readonly {
  readonly kind: DockerKind;
  readonly title: string;
  readonly hint: string;
}[] = [
  {
    kind: "container",
    title: "Containers",
    hint: "A stopped container keeps the files it changed. Removing it loses those; its image stays.",
  },
  {
    kind: "image",
    title: "Images",
    hint: "Downloaded or built images. Docker pulls or builds them again when something needs them.",
  },
  {
    kind: "volume",
    title: "Volumes",
    hint: "Data containers keep, such as databases. Nothing rebuilds it, so none start selected.",
  },
  {
    kind: "build",
    title: "Build cache",
    hint: "Steps saved by docker build so the next build is faster. The next build redoes what is missing.",
  },
];

export function dockerKey(item: Pick<DockerItemDto, "kind" | "reference">): string {
  return `${item.kind}:${item.reference}`;
}

/** What starts selected: what the backend marks, never anything in use. */
export function initialDockerSelection(
  items: readonly DockerItemDto[],
): DockerSelection {
  return new Set(
    items.filter((item) => item.selected && !item.inUse).map(dockerKey),
  );
}

export function toggleDocker(
  selection: DockerSelection,
  item: DockerItemDto,
): DockerSelection {
  if (item.inUse) {
    return selection;
  }
  const next = new Set(selection);
  const key = dockerKey(item);
  if (!next.delete(key)) {
    next.add(key);
  }
  return next;
}

/** The selected items, in listing order, and their bytes. */
export function dockerSelected(
  selection: DockerSelection,
  items: readonly DockerItemDto[],
): { readonly items: readonly DockerItemDto[]; readonly bytes: number } {
  const picked = items.filter(
    (item) => !item.inUse && selection.has(dockerKey(item)),
  );
  return {
    items: picked,
    bytes: picked.reduce((sum, item) => sum + item.bytes, 0),
  };
}
