import assert from "node:assert/strict";
import test from "node:test";

import {
  createNamespaceManifest,
  createNamespaceScope,
  deriveChildNamespaceScope,
  isNamespacePathWithin,
  MAX_NAMESPACE_MOUNTS,
  namespaceMountRoot,
  normalizeNamespacePath,
  resolveNamespaceCwd,
  resolveNamespaceMount,
  routeNamespaceCwd,
} from "nanocodex-tools";

const allRights = [
  "namespace.discover",
  "filesystem.read",
  "filesystem.write",
  "process.exec",
  "process.stdin",
  "network.preview",
];

function manifest() {
  return createNamespaceManifest({
    manifestId: "manifest:stable-snapshot",
    mounts: [
      {
        root: "/laptop",
        mountId: "mount:opaque-laptop",
        handId: "hand:opaque-a",
        exportId: "export:opaque-a",
        generation: "generation:a:7",
        rights: allRights,
      },
      {
        root: "/buildbox",
        mountId: "mount:opaque-buildbox",
        handId: "hand:opaque-b",
        exportId: "export:opaque-b",
        generation: "generation:b:2",
        rights: ["namespace.discover", "filesystem.read", "process.exec"],
      },
    ],
  });
}

test("manifests are canonical immutable snapshots with opaque stable mappings", () => {
  const input = {
    manifestId: "manifest:one",
    mounts: [{
      root: "/zeta",
      mountId: "mount:z",
      handId: "hand:z",
      exportId: "export:z",
      generation: "generation:1",
      rights: ["process.exec"],
    }, {
      root: "/alpha",
      mountId: "mount:a",
      handId: "hand:a",
      exportId: "export:a",
      generation: "generation:9",
      rights: ["filesystem.read", "process.exec"],
    }],
  };
  const snapshot = createNamespaceManifest(input);

  assert.deepEqual(snapshot.mounts.map(({ root }) => root), ["/alpha", "/zeta"]);
  assert.ok(Object.isFrozen(snapshot));
  assert.ok(Object.isFrozen(snapshot.mounts));
  assert.ok(snapshot.mounts.every(Object.isFrozen));
  assert.ok(snapshot.mounts.every(({ rights }) => Object.isFrozen(rights)));
  input.mounts[1].handId = "hand:replaced";
  input.mounts[1].rights.push("filesystem.write");
  assert.equal(snapshot.mounts[0].handId, "hand:a");
  assert.deepEqual(snapshot.mounts[0].rights, ["filesystem.read", "process.exec"]);
  assert.throws(() => { snapshot.mounts[0].root = "/renamed"; }, TypeError);
});

test("manifest admission rejects root ambiguity, aliases, reserved roots, and bounds", () => {
  const mount = (root, mountId = `mount:${root}`) => ({
    root,
    mountId,
    handId: `hand:${root}`,
    exportId: `export:${root}`,
    generation: "generation:1",
    rights: ["process.exec"],
  });
  const make = (mounts) => createNamespaceManifest({ manifestId: "manifest:test", mounts });

  for (const root of ["/.nanocodex", "/dev", "/proc", "/tmp"])
    assert.throws(() => make([mount(root)]), /reserved/);
  assert.doesNotThrow(() => make([mount("/brain"), mount("/sandbox")]));
  for (const root of ["laptop", "/a/b", "/UPPER", "/two words", "/con", "/a/../b", "/a\\b"])
    assert.throws(() => make([mount(root)]), /root|path/);
  assert.throws(() => make([mount("/one"), mount("/one", "mount:two")]), /duplicate mount root/);
  assert.throws(() => make([mount("/one", "same"), mount("/two", "same")]), /duplicate mount identity/);
  assert.throws(
    () => make(Array.from({ length: MAX_NAMESPACE_MOUNTS + 1 }, (_, index) => mount(`/m${index}`))),
    /1 to 64 mounts/,
  );
});

test("machine identities receive deterministic portable non-system roots", () => {
  assert.equal(namespaceMountRoot("laptop"), "/laptop");
  assert.match(namespaceMountRoot("Build Box"), /^\/hand-build-box-[0-9a-f]{8}$/);
  assert.match(namespaceMountRoot("sandbox"), /^\/hand-sandbox-/);
  assert.match(namespaceMountRoot("con"), /^\/hand-con-/);
  assert.equal(namespaceMountRoot("Build Box"), namespaceMountRoot("Build Box"));
  assert.throws(() => namespaceMountRoot(""), /non-empty/);
});

test("normalization resolves cwd and rejects traversal and host-dependent separators", () => {
  assert.equal(normalizeNamespacePath("//laptop/./repo///src/../test"), "/laptop/repo/test");
  assert.equal(resolveNamespaceCwd("/laptop/repo/src", "../test"), "/laptop/repo/test");
  assert.equal(resolveNamespaceCwd("/laptop/repo", "/buildbox/work"), "/buildbox/work");
  assert.equal(resolveNamespaceCwd("/laptop/repo", ""), "/laptop/repo");
  assert.throws(() => normalizeNamespacePath("relative/path"), /must be absolute/);
  assert.throws(() => normalizeNamespacePath("/../../escape"), /escapes logical root/);
  assert.throws(() => normalizeNamespacePath("/laptop/evil\0path"), /non-portable/);
  assert.throws(() => normalizeNamespacePath("/laptop\\..\\buildbox"), /non-portable/);
  assert.equal(normalizeNamespacePath("/laptop/%2e%2e/buildbox"), "/laptop/%2e%2e/buildbox");
});

test("mount resolution uses segment boundaries and retains the admitted identity", () => {
  const snapshot = manifest();
  const laptop = resolveNamespaceMount(snapshot, "/laptop/repo");
  assert.equal(laptop.mountId, "mount:opaque-laptop");
  assert.equal(laptop.generation, "generation:a:7");
  assert.equal(resolveNamespaceMount(snapshot, "/laptop2/repo"), undefined);
  assert.equal(resolveNamespaceMount(snapshot, "/"), undefined);
  assert.equal(isNamespacePathWithin("/laptop/repo", "/laptop"), true);
  assert.equal(isNamespacePathWithin("/laptop2/repo", "/laptop"), false);
});

test("cwd routing is stable, relative, right-checked, and never inferred from names", () => {
  const snapshot = manifest();
  const scope = createNamespaceScope(snapshot, "/laptop/repo");
  const route = routeNamespaceCwd(scope, "src/../test");
  assert.deepEqual({
    cwd: route.cwd,
    relativePath: route.relativePath,
    mountId: route.mount.mountId,
    generation: route.mount.generation,
  }, {
    cwd: "/laptop/repo/test",
    relativePath: "/repo/test",
    mountId: "mount:opaque-laptop",
    generation: "generation:a:7",
  });
  assert.ok(Object.isFrozen(route));

  const readOnly = deriveChildNamespaceScope(scope, [{
    mountId: "mount:opaque-buildbox",
    path: "/buildbox/repo",
    rights: ["filesystem.read"],
  }, {
    mountId: "mount:opaque-laptop",
    path: "/laptop/repo",
    rights: ["filesystem.read"],
  }]);
  assert.throws(() => routeNamespaceCwd(readOnly), /lacks process.exec/);
  assert.equal(routeNamespaceCwd(readOnly, undefined, "filesystem.read").mount.handId, "hand:opaque-a");
  assert.throws(
    () => routeNamespaceCwd({
      manifest: snapshot,
      defaultCwd: "/laptop/repo",
      grants: [{ mountId: "mount:opaque-laptop", path: "/laptop", rights: allRights }],
    }),
    /scope was not admitted/,
  );
});

test("children and grandchildren can only attenuate identity, subtree, and rights", () => {
  const parent = createNamespaceScope(manifest(), "/laptop/repo");
  const child = deriveChildNamespaceScope(parent, [{
    mountId: "mount:opaque-laptop",
    path: "repo",
    rights: ["filesystem.read", "process.exec"],
  }]);
  assert.ok(Object.isFrozen(child));
  assert.ok(Object.isFrozen(child.grants));
  assert.deepEqual(child.grants[0].rights, ["filesystem.read", "process.exec"]);
  assert.equal(routeNamespaceCwd(child, "src").cwd, "/laptop/repo/src");
  assert.throws(() => routeNamespaceCwd(child, "/laptop/other"), /lacks process.exec/);
  assert.throws(() => deriveChildNamespaceScope(child, [{
    mountId: "mount:opaque-laptop",
    path: "/laptop",
    rights: ["filesystem.read"],
  }]), /exceed|inherited cwd/);
  assert.throws(() => deriveChildNamespaceScope(child, [{
    mountId: "mount:opaque-laptop",
    path: "/laptop/repo",
    rights: ["filesystem.write"],
  }]), /exceed/);
  assert.throws(() => deriveChildNamespaceScope(child, [{
    mountId: "mount:opaque-buildbox",
    path: "/buildbox",
    rights: ["filesystem.read"],
  }]), /exceed|inherited cwd/);

  assert.throws(() => deriveChildNamespaceScope(child, [{
    mountId: "mount:opaque-laptop",
    path: "/laptop/repo/src",
    rights: ["filesystem.read"],
  }]), /inherited cwd/);
});

test("attenuation rejects an inaccessible inherited cwd atomically", () => {
  const parent = createNamespaceScope(manifest(), "/laptop/repo");
  assert.throws(() => deriveChildNamespaceScope(parent, []), /inherited cwd/);
  assert.throws(() => deriveChildNamespaceScope(parent, [{
    mountId: "mount:opaque-buildbox",
    path: "/buildbox/project",
    rights: ["filesystem.read"],
  }]), /inherited cwd/);
  assert.throws(() => deriveChildNamespaceScope(parent, [{
    mountId: "/laptop",
    rights: ["process.exec"],
  }]), /inaccessible mount/);

  const inherited = deriveChildNamespaceScope(parent);
  assert.notEqual(inherited, parent);
  assert.equal(routeNamespaceCwd(inherited).mount.mountId, "mount:opaque-laptop");
});
