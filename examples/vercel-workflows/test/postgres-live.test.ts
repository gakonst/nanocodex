import { randomUUID } from "node:crypto";

import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { Pool } from "pg";

import {
  DurabilityImportConflictError,
  durabilityRevision,
  exportDurabilityState,
  importDurabilityState,
} from "nanocodex/durability";
import {
  createPostgresDurabilityStore,
  type PostgresDurabilityClient,
  type PostgresDurabilityPool,
  type PostgresDurabilityQueryResult,
  type PostgresDurabilityRow,
} from "nanocodex/durability/postgres";

const live = process.env.NANOCODEX_LIVE_POSTGRES === "1";
const connectionString = process.env.DATABASE_URL;
const schemaA = `nanocodex_live_a_${randomUUID().replaceAll("-", "")}`;
const schemaB = `nanocodex_live_b_${randomUUID().replaceAll("-", "")}`;
let admin: Pool;
let sourcePool: Pool;
let destinationPool: Pool;

describe.runIf(live)("live PostgreSQL durability", () => {
  beforeAll(async () => {
    if (!connectionString) throw new Error("DATABASE_URL is required for live PostgreSQL tests");
    admin = new Pool({ connectionString });
    await admin.query(`CREATE SCHEMA ${schemaA}`);
    await admin.query(`CREATE SCHEMA ${schemaB}`);
    sourcePool = new Pool({ connectionString, options: `-c search_path=${schemaA}` });
    destinationPool = new Pool({ connectionString, options: `-c search_path=${schemaB}` });
  });

  afterAll(async () => {
    await Promise.allSettled([sourcePool?.end(), destinationPool?.end()]);
    if (admin) {
      await admin.query(`DROP SCHEMA IF EXISTS ${schemaA} CASCADE`);
      await admin.query(`DROP SCHEMA IF EXISTS ${schemaB} CASCADE`);
      await admin.end();
    }
  });

  it("uses real pg.Pool transactions for fencing, CAS, and portable import", async () => {
    const stateId = `live-postgres-${randomUUID()}`;
    const source = createPostgresDurabilityStore(sourcePool);
    const owner = await source.acquire(stateId, { ownerId: "live-owner" });
    const contenders = await Promise.all(Array.from({ length: 24 }, (_, index) => (
      createPostgresDurabilityStore(sourcePool).replace(stateId, {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: owner.revision,
        payload: `winner-${index}`,
      })
    )));
    expect(contenders.filter(({ status }) => status === "replaced")).toHaveLength(1);
    expect(contenders.filter(({ status }) => status === "conflict")).toHaveLength(23);

    const retained = await source.load(stateId);
    expect(retained).toMatchObject({ revision: durabilityRevision("1") });
    const archive = JSON.parse(JSON.stringify(
      await exportDurabilityState(source, stateId),
    ));
    const destination = createPostgresDurabilityStore(destinationPool);
    await expect(importDurabilityState(destination, archive)).resolves.toEqual(retained);
    await expect(importDurabilityState(destination, archive)).rejects.toBeInstanceOf(
      DurabilityImportConflictError,
    );

    const reopened = await destination.acquire(stateId, { ownerId: "vercel-workflow-step" });
    expect(reopened).toMatchObject(retained);
    await expect(destination.replace(stateId, {
      ownerId: reopened.ownerId,
      fence: reopened.fence,
      expectedRevision: reopened.revision,
      payload: `${retained.payload}:continued-on-vercel`,
    })).resolves.toEqual({ status: "replaced", revision: durabilityRevision("2") });
    await expect(source.replace(stateId, {
      ownerId: owner.ownerId,
      fence: owner.fence,
      expectedRevision: durabilityRevision("1"),
      payload: "stale-source-resurrection",
    })).resolves.toEqual({ status: "fenced" });
  });

  it("serializes two empty-target imports before expected-state validation", async () => {
    if (!connectionString) throw new Error("DATABASE_URL is required for live PostgreSQL tests");
    const stateId = `live-import-race-${randomUUID()}`;
    const firstApplication = `nanocodex-import-first-${randomUUID()}`;
    const secondApplication = `nanocodex-import-second-${randomUUID()}`;
    const firstPool = new Pool({
      application_name: firstApplication,
      connectionString,
      max: 1,
      options: `-c search_path=${schemaB}`,
    });
    const secondPool = new Pool({
      application_name: secondApplication,
      connectionString,
      max: 1,
      options: `-c search_path=${schemaB}`,
    });
    const firstLocked = deferred();
    const releaseFirst = deferred();
    let firstAttempt: Promise<unknown> | undefined;
    let secondAttempt: Promise<unknown> | undefined;

    try {
      const first = createPostgresDurabilityStore(
        pauseAfterStateLock(firstPool, stateId, firstLocked.resolve, releaseFirst.promise),
      );
      const second = createPostgresDurabilityStore(secondPool);
      await Promise.all([first.load("initialize-first"), second.load("initialize-second")]);

      firstAttempt = Promise.resolve(first.importState(stateId, {
        revision: durabilityRevision("8"),
        payload: "first-import",
      }, {
        expectedRevision: durabilityRevision("0"),
        expectedPayload: null,
      }));
      await firstLocked.promise;
      secondAttempt = Promise.resolve(second.importState(stateId, {
        revision: durabilityRevision("9"),
        payload: "second-import",
      }, {
        expectedRevision: durabilityRevision("0"),
        expectedPayload: null,
      }));
      await waitForAdvisoryLock(secondApplication);
      releaseFirst.resolve();

      const attempts = await Promise.allSettled([firstAttempt, secondAttempt]);
      expect(attempts[0]).toEqual({
        status: "fulfilled",
        value: { revision: durabilityRevision("8"), payload: "first-import" },
      });
      expect(attempts[1]).toMatchObject({ status: "rejected" });
      if (attempts[1]?.status === "rejected") {
        expect(attempts[1].reason).toBeInstanceOf(DurabilityImportConflictError);
        expect(attempts[1].reason).toMatchObject({
          expectedRevision: durabilityRevision("0"),
          actualRevision: durabilityRevision("8"),
        });
      }
      await expect(first.load(stateId)).resolves.toEqual({
        revision: durabilityRevision("8"),
        payload: "first-import",
      });
    } finally {
      releaseFirst.resolve();
      await Promise.allSettled([firstAttempt, secondAttempt].filter((attempt) => attempt !== undefined));
      await Promise.allSettled([firstPool.end(), secondPool.end()]);
    }
  });
});

function pauseAfterStateLock(
  pool: Pool,
  stateId: string,
  locked: () => void,
  release: Promise<void>,
): PostgresDurabilityPool {
  return {
    async connect(): Promise<PostgresDurabilityClient> {
      const client = await pool.connect();
      return {
        async query<Row extends PostgresDurabilityRow = PostgresDurabilityRow>(
          text: string,
          values: unknown[] = [],
        ): Promise<PostgresDurabilityQueryResult<Row>> {
          const result = await client.query(text, values);
          if (text.includes(":nanocodex-durability-v2:state:") && values[0] === stateId) {
            locked();
            await release;
          }
          return { rows: result.rows as unknown as readonly Row[] };
        },
        release(discard?: Error | boolean): void {
          client.release(discard);
        },
      };
    },
    async query<Row extends PostgresDurabilityRow = PostgresDurabilityRow>(
      text: string,
      values: unknown[] = [],
    ): Promise<PostgresDurabilityQueryResult<Row>> {
      const result = await pool.query(text, values);
      return { rows: result.rows as unknown as readonly Row[] };
    },
  };
}

async function waitForAdvisoryLock(applicationName: string): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    const result = await admin.query<{ waiting: boolean }>(
      `SELECT EXISTS (
         SELECT 1 FROM pg_stat_activity
         WHERE application_name = $1
           AND wait_event_type = 'Lock'
           AND wait_event = 'advisory'
       ) AS waiting`,
      [applicationName],
    );
    if (result.rows[0]?.waiting) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`PostgreSQL client ${JSON.stringify(applicationName)} did not wait for the state lock`);
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((accept) => { resolve = accept; });
  return { promise, resolve };
}
