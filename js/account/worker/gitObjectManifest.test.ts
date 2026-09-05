import assert from "node:assert/strict";
import { test } from "node:test";

import {
  gitObjectType,
  isGitObjectManifest,
  selectGitObjects,
  type GitObjectManifest,
  type GitObjectRecord,
} from "./gitObjectManifest.ts";

const oid = (character: string) => character.repeat(40);
const c1 = oid("1");
const c2 = oid("2");
const c3 = oid("3");
const t1 = oid("4");
const t2 = oid("5");
const t3 = oid("6");
const b1 = oid("7");
const b2 = oid("8");
const b3 = oid("9");

const dependencies = new Map<string, [number, string[]]>([
  [c1, [gitObjectType.commit, [t1]]],
  [c2, [gitObjectType.commit, [t2, c1]]],
  [c3, [gitObjectType.commit, [t3, c2]]],
  [t1, [gitObjectType.tree, [b1]]],
  [t2, [gitObjectType.tree, [b1, b2]]],
  [t3, [gitObjectType.tree, [b1, b2, b3]]],
  [b1, [gitObjectType.blob, []]],
  [b2, [gitObjectType.blob, []]],
  [b3, [gitObjectType.blob, []]],
]);

const objects = Object.fromEntries([...dependencies].map(([id, [type, children]], index) => [
  id,
  [type, 0, index * 10, 10, children] as GitObjectRecord,
]));
const manifest: GitObjectManifest = {
  version: 1,
  head: c3,
  shards: [{ key: `generations/${c3}/objects/0000.pack`, size: 1_000 }],
  objects,
};

test("object manifests validate every shard range and dependency", () => {
  assert.equal(isGitObjectManifest(manifest), true);
  assert.equal(isGitObjectManifest({
    ...manifest,
    objects: { ...objects, [b3]: [gitObjectType.blob, 0, 999, 10, []] },
  }), false);
});

test("incremental selection excludes the complete closure of client haves", () => {
  const selection = selectGitObjects(manifest, [c3], [c2], [], 0);
  assert.deepEqual(new Set(selection.objectIds), new Set([c3, t3, b3]));
  assert.deepEqual(selection.shallow, []);
  assert.deepEqual(selection.unshallow, []);
});

test("shallow selection cuts commits at depth and deepens existing boundaries", () => {
  const clone = selectGitObjects(manifest, [c3], [], [], 1);
  assert.deepEqual(new Set(clone.objectIds), new Set([c3, t3, b1, b2, b3]));
  assert.deepEqual(clone.shallow, [c3]);

  const deepen = selectGitObjects(manifest, [c3], [c3], [c3], 2);
  assert.deepEqual(new Set(deepen.objectIds), new Set([c2, t2]));
  assert.deepEqual(deepen.shallow, [c2]);
  assert.deepEqual(deepen.unshallow, [c3]);

  const relative = selectGitObjects(manifest, [c3], [c3], [c3], 1, true);
  assert.deepEqual(new Set(relative.objectIds), new Set([c2, t2]));
  assert.deepEqual(relative.shallow, [c2]);
  assert.deepEqual(relative.unshallow, [c3]);
});
