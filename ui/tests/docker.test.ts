// The Docker section's selection. Run with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { DockerItemDto, DockerKind } from "../src/lib/api.ts";
import {
  dockerSelected,
  initialDockerSelection,
  toggleDocker,
} from "../src/lib/docker.ts";

function item(
  kind: DockerKind,
  reference: string,
  bytes: number,
  selected: boolean,
  inUse = false,
): DockerItemDto {
  return {
    kind,
    reference,
    name: reference,
    detail: "",
    bytes,
    inUse,
    when: null,
    selected,
  };
}

const items = [
  item("container", "c1", 10, false, true),
  item("image", "node:20", 100, true),
  item("volume", "orphan", 1000, false),
  item("build", "build-cache-old", 5000, true),
  item("image", "busy", 7, true, true),
];

test("starts with what the backend selects, never anything in use", () => {
  const selection = initialDockerSelection(items);
  assert.deepEqual([...selection], ["image:node:20", "build:build-cache-old"]);
  assert.equal(dockerSelected(selection, items).bytes, 5100);
});

test("toggling adds and removes, but cannot pick what is in use", () => {
  let selection = initialDockerSelection(items);
  selection = toggleDocker(selection, items[2]!);
  selection = toggleDocker(selection, items[1]!);
  selection = toggleDocker(selection, items[0]!);
  const picked = dockerSelected(selection, items);
  assert.deepEqual(
    picked.items.map((entry) => entry.reference),
    ["orphan", "build-cache-old"],
  );
  assert.equal(picked.bytes, 6000);
});
