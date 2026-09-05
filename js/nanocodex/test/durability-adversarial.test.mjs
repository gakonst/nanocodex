import assert from "node:assert/strict";
import test from "node:test";

import {
  DurabilityImportConflictError,
  createMemoryDurabilityStore,
  durabilityRevision,
  durabilityStateDigest,
  exportDurabilityState,
  exportDurabilityStatePage,
  importDurabilityState,
  importDurabilityStatePages,
} from "nanocodex/durability";
import {
  acquire,
  own,
  release,
  replace,
  retain,
} from "../runtime/durability.mjs";

test("durability rejects malformed identities, counters, payloads, and old state shapes", async () => {
  for (const id of ["", "   ", null, 1, {}, []]) {
    assert.throws(() => createMemoryDurabilityStore(id), /state ID must be a non-empty string/);
  }
  for (const counter of ["", "00", "-1", "+1", "1.0", " 1", 1.5, -1, {}, null]) {
    assert.throws(() => durabilityRevision(counter), /durability revision/);
  }
  assert.throws(
    () => createMemoryDurabilityStore("state", { revision: "0", payload: "impossible" }),
    /payload must be null exactly at revision zero/,
  );
  assert.throws(
    () => createMemoryDurabilityStore("state", { revision: "1", payload: null }),
    /payload must be null exactly at revision zero/,
  );
  assert.throws(
    () => createMemoryDurabilityStore("state", { revision: "1", batches: [] }),
    /exactly payload and revision/,
  );
  assert.throws(
    () => createMemoryDurabilityStore("state", { revision: "1", payload: "x", extra: true }),
    /exactly payload and revision/,
  );
});

test("the host boundary rejects every malformed acquired-state projection", async () => {
  const invalid = [
    null,
    [],
    { ownerId: "owner", fence: "1", revision: "0", payload: "present" },
    { ownerId: "owner", fence: "1", revision: "1", payload: null },
    { ownerId: "owner", fence: "01", revision: "0", payload: null },
    { ownerId: "other", fence: "1", revision: "0", payload: null },
    { ownerId: "owner", fence: "18446744073709551616", revision: "0", payload: null },
    { ownerId: "owner", fence: "1", revision: "18446744073709551616", payload: "x" },
    { ownerId: "owner", fence: "1", revision: "1", payload: new Uint8Array() },
    { ownerId: "owner", fence: "1", revision: "0", payload: null, batches: [] },
  ];
  for (const [index, acquired] of invalid.entries()) {
    const host = {};
    const route = own(host, {
      acquire: () => acquired,
      replace: () => ({ status: "fenced" }),
    }, `state-${index}`);
    retain(host, route.id);
    await assert.rejects(acquire(route.id, `state-${index}`, "owner"), /durability/);
    release(host, route.id);
  }
});

test("the host boundary accepts only the four exact replace outcomes", async () => {
  const outcomes = [
    undefined,
    null,
    {},
    { status: "appended", revision: "1" },
    { status: "replaced", revision: "01" },
    { status: "conflict", actualRevision: "-1" },
    { status: "not_committed", message: "" },
    { status: "unknown" },
    { status: "fenced", revision: "1" },
    { status: "replaced", revision: "1", appended: true },
  ];
  for (const [index, outcome] of outcomes.entries()) {
    const host = {};
    const route = own(host, {
      acquire: () => ({ ownerId: "owner", fence: "1", revision: "0", payload: null }),
      replace: () => outcome,
    }, `state-${index}`);
    retain(host, route.id);
    await assert.rejects(
      replace(route.id, `state-${index}`, "owner", "1", "0", "payload"),
      /durability/,
    );
    release(host, route.id);
  }
});

test("stale owners lose before revision comparison and replacement is total", () => {
  const store = createMemoryDurabilityStore("state");
  const first = store.acquire("state", { ownerId: "first" });
  assert.deepEqual(store.replace("state", {
    ...first,
    expectedRevision: "0",
    payload: "first-state",
  }), { status: "replaced", revision: "1" });
  const second = store.acquire("state", { ownerId: "second" });
  assert.deepEqual(store.replace("state", {
    ...first,
    expectedRevision: "999",
    payload: "stale",
  }), { status: "fenced" });
  assert.deepEqual(store.replace("state", {
    ...second,
    expectedRevision: "1",
    payload: "second-state",
  }), { status: "replaced", revision: "2" });
  assert.deepEqual(store.snapshot(), { revision: "2", payload: "second-state" });
  assert.equal("append" in store, false);
  assert.equal("compact" in store, false);
  assert.equal("acquirePage" in store, false);
});

test("portable export fences the source and exact import refuses overwrite", async () => {
  const source = createMemoryDurabilityStore("source");
  const sourceOwner = source.acquire("source", { ownerId: "source-owner" });
  assert.deepEqual(source.replace("source", {
    ...sourceOwner,
    expectedRevision: "0",
    payload: "opaque-total-state",
  }), { status: "replaced", revision: "1" });

  const archive = await exportDurabilityState(source, "source");
  assert.deepEqual(archive, {
    format: "nanocodex-durability-state-v1",
    stateId: "source",
    revision: "1",
    payload: "opaque-total-state",
  });
  assert.deepEqual(source.replace("source", {
    ...sourceOwner,
    expectedRevision: "1",
    payload: "split-brain-write",
  }), { status: "fenced" });

  const serialized = JSON.parse(JSON.stringify(archive));
  const destination = createMemoryDurabilityStore("source");
  assert.deepEqual(
    await importDurabilityState(destination, serialized),
    { revision: "1", payload: "opaque-total-state" },
  );
  assert.deepEqual(destination.load("source"), {
    revision: "1",
    payload: "opaque-total-state",
  });
  await assert.rejects(
    importDurabilityState(destination, archive),
    DurabilityImportConflictError,
  );
});

test("cursor export resumes only from the exact from-state lineage", async () => {
  const payload = "first-page:🧪:middle:second-page:tail";
  const fromPayload = "older-total-state";
  const fromDigest = await durabilityStateDigest({ revision: "4", payload: fromPayload });
  const source = createMemoryDurabilityStore("range", { revision: "9", payload });
  const first = await exportDurabilityStatePage(source, "range", {
    from: "4",
    fromDigest,
    limit: 12,
  });
  assert.equal(first.from, "4");
  assert.match(first.fromDigest, /^sha256:[0-9a-f]{64}$/);
  assert.equal(first.to, "9");
  assert.equal(first.cursor, "v1:0");
  assert.notEqual(first.nextCursor, null);

  const repeated = await exportDurabilityStatePage(source, "range", {
    from: "4",
    fromDigest,
    to: first.to,
    cursor: first.cursor,
    limit: 12,
  });
  assert.deepEqual(repeated, first, "retrying a cursor returns the identical page");

  const pages = [first];
  while (pages.at(-1).nextCursor !== null) {
    pages.push(await exportDurabilityStatePage(source, "range", {
      from: "4",
      fromDigest,
      to: first.to,
      cursor: pages.at(-1).nextCursor,
      limit: 12,
    }));
  }
  assert.equal(pages.map((page) => page.payload).join(""), payload);

  const destination = createMemoryDurabilityStore("range", {
    revision: "4",
    payload: fromPayload,
  });
  assert.deepEqual(await importDurabilityStatePages(destination, pages), {
    revision: "9",
    payload,
  });
  assert.deepEqual(destination.snapshot(), { revision: "9", payload });

  const sameRevisionDivergence = createMemoryDurabilityStore("range", {
    revision: "4",
    payload: "unrelated-lineage-at-the-same-revision",
  });
  await assert.rejects(
    importDurabilityStatePages(sameRevisionDivergence, pages),
    (error) => error instanceof DurabilityImportConflictError
      && error.expectedRevision === "4"
      && error.actualRevision === "4",
  );
  assert.deepEqual(sameRevisionDivergence.snapshot(), {
    revision: "4",
    payload: "unrelated-lineage-at-the-same-revision",
  });

  const directGuard = createMemoryDurabilityStore("range", {
    revision: "4",
    payload: fromPayload,
  });
  assert.throws(
    () => directGuard.importState("range", { revision: "9", payload }, {
      expectedRevision: "4",
      expectedPayload: "unrelated-lineage-at-the-same-revision",
    }),
    /different payload lineage/,
  );
  assert.deepEqual(directGuard.snapshot(), { revision: "4", payload: fromPayload });

  const divergent = createMemoryDurabilityStore("range", {
    revision: "5",
    payload: "diverged",
  });
  await assert.rejects(
    importDurabilityStatePages(divergent, pages),
    (error) => error instanceof DurabilityImportConflictError
      && error.expectedRevision === "4"
      && error.actualRevision === "5",
  );
  await assert.rejects(
    importDurabilityStatePages(createMemoryDurabilityStore("range"), pages.slice(1)),
    /missing, duplicated, or out of order/,
  );
  await assert.rejects(
    exportDurabilityStatePage(source, "range", { from: "4", limit: 12 }),
    /require a from-state digest/,
  );
  const mixedLineage = pages.map((page, index) => index === 1
    ? { ...page, fromDigest: `sha256:${"0".repeat(64)}` }
    : page);
  await assert.rejects(
    importDurabilityStatePages(createMemoryDurabilityStore("range", {
      revision: "4",
      payload: fromPayload,
    }), mixedLineage),
    /different revision ranges/,
  );
  const uniformlyTamperedLineage = pages.map((page) => ({
    ...page,
    fromDigest: `sha256:${"0".repeat(64)}`,
  }));
  await assert.rejects(
    importDurabilityStatePages(createMemoryDurabilityStore("range", {
      revision: "4",
      payload: fromPayload,
    }), uniformlyTamperedLineage),
    /different payload lineage/,
  );
});

test("portable import rejects malformed or unsupported archives", async () => {
  const destination = createMemoryDurabilityStore("destination");
  await assert.rejects(
    importDurabilityState(destination, {
      format: "nanocodex-durability-state-v0",
      stateId: "source",
      revision: "1",
      payload: "state",
    }),
    /unsupported durability export format/,
  );
  await assert.rejects(
    importDurabilityState(destination, {
      format: "nanocodex-durability-state-v1",
      stateId: "source",
      revision: "0",
      payload: "impossible",
    }),
    /payload must be null exactly at revision zero/,
  );
});

test("portable import reserves an empty revision and never fences an initialized target", async () => {
  const source = createMemoryDurabilityStore("empty-portable");
  const archive = await exportDurabilityState(source, "empty-portable");
  const destination = createMemoryDurabilityStore("empty-portable");

  assert.deepEqual(await importDurabilityState(destination, archive), {
    revision: "0",
    payload: null,
  });
  await assert.rejects(importDurabilityState(destination, archive), DurabilityImportConflictError);

  const initialized = createMemoryDurabilityStore("empty-portable");
  initialized.acquire("empty-portable", { ownerId: "live-owner" });
  await assert.rejects(importDurabilityState(initialized, archive), DurabilityImportConflictError);
});

test("portable cutover has no random-number-generator dependency", async () => {
  const cryptoDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crypto");
  Object.defineProperty(globalThis, "crypto", {
    configurable: true,
    value: undefined,
  });
  try {
    const source = createMemoryDurabilityStore("crypto-free");
    const archive = await exportDurabilityState(source, "crypto-free");
    const destination = createMemoryDurabilityStore("crypto-free");
    assert.deepEqual(await importDurabilityState(destination, archive), {
      revision: "0",
      payload: null,
    });
  } finally {
    if (cryptoDescriptor) Object.defineProperty(globalThis, "crypto", cryptoDescriptor);
    else delete globalThis.crypto;
  }
});


test("large Unicode acquired states cross as exact UTF-8 without a nested JSON envelope", async () => {
  const payload = 'retained \" 🧪\n'.repeat(100_000);
  const host = {};
  const store = createMemoryDurabilityStore("unicode-state", { revision: "1", payload });
  const route = own(host, store, "unicode-state");
  retain(host, route.id);
  try {
    const acquired = await acquire(route.id, "unicode-state", "owner");
    assert.equal(acquired.owner_id, "owner");
    assert.equal(acquired.revision, "1");
    assert.ok(acquired.payload instanceof Uint8Array);
    assert.equal(new TextDecoder().decode(acquired.payload), payload);
    assert.equal(store.load("unicode-state").payload, payload);
  } finally { release(host, route.id); }
});
