import { readFile } from "node:fs/promises";

import { PGlite } from "@electric-sql/pglite";
import { describe, expect, it } from "vitest";
import { WebSocketServer, type RawData, type WebSocket } from "ws";

import {
  DurabilityImportConflictError,
  durabilityRevision,
  durabilityStateDigest,
  exportDurabilityStatePage,
  importDurabilityStatePages,
  type DurabilityPortableStatePage,
} from "nanocodex/durability";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import {
  createPostgresDurabilityStore,
  type PostgresDurabilityClient,
  type PostgresDurabilityPool,
  type PostgresDurabilityQueryResult,
  type PostgresDurabilityRow,
} from "nanocodex/durability/postgres";
import { Agent, Transport } from "nanocodex/node";
import { cloudflareDurabilityStorage } from "./cloudflare-durability-storage";
import { postgresDurabilityStore } from "../workflows/postgres-durability";

const MAX_REVISION = durabilityRevision("18446744073709551615");
const BEFORE_MAX_REVISION = durabilityRevision("18446744073709551614");
type NodeAgent = Awaited<ReturnType<typeof Agent.create>>;

describe("Vercel PostgreSQL durability store", () => {
  it("does not require DATABASE_URL until the application store is requested", () => {
    const original = process.env.DATABASE_URL;
    delete process.env.DATABASE_URL;
    try {
      expect(() => postgresDurabilityStore()).toThrow("DATABASE_URL is not configured");
    } finally {
      if (original === undefined) delete process.env.DATABASE_URL;
      else process.env.DATABASE_URL = original;
    }
  });

  it("guards independent cold schema initializers with the PostgreSQL advisory lock", async () => {
    const pool = new PGlitePool();
    try {
      const first = createPostgresDurabilityStore(pool.asPostgresPool());
      const second = createPostgresDurabilityStore(pool.asPostgresPool());
      await expect(Promise.all([
        first.load("schema-a"),
        second.load("schema-b"),
      ])).resolves.toEqual([
        { revision: durabilityRevision("0"), payload: null },
        { revision: durabilityRevision("0"), payload: null },
      ]);
      expect(pool.clientQueries.filter((query) => query.startsWith(
        "SELECT pg_advisory_xact_lock",
      ))).toHaveLength(2);
    } finally {
      await pool.close();
    }
  });

  it("reopens one complete state with a higher owner fence", async () => {
    const pool = new PGlitePool();
    try {
      const first = createPostgresDurabilityStore(pool.asPostgresPool());
      const owner = await first.acquire("shared", { ownerId: "first-owner" });
      expect(owner).toEqual({
        ownerId: "first-owner",
        fence: durabilityRevision("1"),
        revision: durabilityRevision("0"),
        payload: null,
      });
      await expect(first.replace("shared", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: owner.revision,
        payload: "written-by-js",
      })).resolves.toEqual({ status: "replaced", revision: durabilityRevision("1") });

      const reopened = createPostgresDurabilityStore(pool.asPostgresPool());
      await expect(reopened.acquire("shared", { ownerId: "second-owner" })).resolves.toEqual({
        ownerId: "second-owner",
        fence: durabilityRevision("2"),
        revision: durabilityRevision("1"),
        payload: "written-by-js",
      });
      await expect(pool.query(
        `INSERT INTO nanocodex_durable_owners (state_id, owner_id, fence)
         VALUES ('invalid-zero-fence', 'invalid', 0)`,
      )).rejects.toThrow();
    } finally {
      await pool.close();
    }
  });

  it("ignores retained journal tables during the hard cutover", async () => {
    const pool = new PGlitePool();
    try {
      await pool.query(
        `CREATE TABLE nanocodex_journals (
           journal_id TEXT PRIMARY KEY,
           revision NUMERIC(20, 0) NOT NULL
         )`,
      );
      await pool.query(
        `INSERT INTO nanocodex_journals (journal_id, revision) VALUES ('legacy', 7)`,
      );
      const store = createPostgresDurabilityStore(pool.asPostgresPool());
      await expect(store.load("legacy")).resolves.toEqual({
        revision: durabilityRevision("0"),
        payload: null,
      });
      await expect(pool.query(
        "SELECT revision::text AS revision FROM nanocodex_journals WHERE journal_id = 'legacy'",
      )).resolves.toEqual({ rows: [{ revision: "7" }] });
    } finally {
      await pool.close();
    }
  });

  it("chooses one of many independent complete-state CAS contenders", async () => {
    const pool = new PGlitePool();
    try {
      const stores = Array.from(
        { length: 16 },
        () => createPostgresDurabilityStore(pool.asPostgresPool()),
      );
      await Promise.all(stores.map((store, index) => store.load(`schema-${index}`)));
      const owner = await stores[0]!.acquire("race", { ownerId: "race-owner" });
      const contenders = await Promise.all(stores.map((store, index) => store.replace("race", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: durabilityRevision("0"),
        payload: `state-${index}`,
      })));
      expect(contenders.filter((result) => result.status === "replaced")).toEqual([
        { status: "replaced", revision: durabilityRevision("1") },
      ]);
      expect(contenders.filter((result) => result.status === "conflict")).toEqual(
        Array.from({ length: 15 }, () => ({
          status: "conflict",
          actualRevision: durabilityRevision("1"),
        })),
      );
      const winner = contenders.findIndex((result) => result.status === "replaced");
      await expect(stores[0]!.load("race")).resolves.toEqual({
        revision: durabilityRevision("1"),
        payload: `state-${winner}`,
      });
      await expect(pool.query<{ state_count: string }>(
        "SELECT count(*)::text AS state_count FROM nanocodex_durable_states WHERE state_id = $1",
        ["race"],
      )).resolves.toEqual({ rows: [{ state_count: "1" }] });
    } finally {
      await pool.close();
    }
  });

  it("admits exactly one concurrent portable import into an empty PostgreSQL target", async () => {
    const pool = new PGlitePool();
    try {
      const stores = [
        createPostgresDurabilityStore(pool.asPostgresPool()),
        createPostgresDurabilityStore(pool.asPostgresPool()),
      ];
      const attempts = await Promise.allSettled(stores.map((store) => store.importState(
        "portable-race",
        { revision: durabilityRevision("23"), payload: "one-exact-import" },
      )));
      expect(attempts.filter(({ status }) => status === "fulfilled")).toHaveLength(1);
      const rejected = attempts.find(({ status }) => status === "rejected");
      expect(rejected).toMatchObject({ status: "rejected" });
      if (rejected?.status === "rejected") {
        expect(rejected.reason).toBeInstanceOf(DurabilityImportConflictError);
      }
      await expect(stores[0]!.load("portable-race")).resolves.toEqual({
        revision: durabilityRevision("23"),
        payload: "one-exact-import",
      });
    } finally {
      await pool.close();
    }
  });

  it("preserves the complete unsigned-u64 decimal range without JS numbers", async () => {
    const pool = new PGlitePool();
    try {
      const store = createPostgresDurabilityStore(pool.asPostgresPool());
      const owner = await store.acquire("u64", { ownerId: "u64-owner" });
      await pool.query(
        `INSERT INTO nanocodex_durable_states (state_id, revision, payload)
         VALUES ($1, $2::numeric, $3)`,
        ["u64", BEFORE_MAX_REVISION, "before-max"],
      );

      await expect(store.replace("u64", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: BEFORE_MAX_REVISION,
        payload: "max-state",
      })).resolves.toEqual({ status: "replaced", revision: MAX_REVISION });
      await expect(store.load("u64")).resolves.toEqual({
        revision: MAX_REVISION,
        payload: "max-state",
      });
      await expect(store.replace("u64", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: MAX_REVISION,
        payload: "overflow",
      })).resolves.toEqual({
        status: "not_committed",
        message: "PostgreSQL durability revision overflow",
      });
    } finally {
      await pool.close();
    }
  });

  it("reconciles a lost COMMIT response to one definitive successful write", async () => {
    const pool = new PGlitePool({ failCommitAfter: 3 });
    try {
      const first = createPostgresDurabilityStore(pool.asPostgresPool());
      const owner = await first.acquire("unknown", { ownerId: "first-owner" });
      await expect(first.replace("unknown", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: durabilityRevision("0"),
        payload: "committed-before-disconnect",
      })).resolves.toEqual({ status: "replaced", revision: durabilityRevision("1") });

      const recreated = createPostgresDurabilityStore(pool.asPostgresPool());
      await expect(recreated.acquire("unknown", { ownerId: "recreated-owner" })).resolves.toEqual({
        ownerId: "recreated-owner",
        fence: durabilityRevision("2"),
        revision: durabilityRevision("1"),
        payload: "committed-before-disconnect",
      });
      expect(pool.releases.filter(Boolean)).toHaveLength(1);
    } finally {
      await pool.close();
    }
  });

  it("distinguishes transactions that never began from rolled-back state writes", async () => {
    const pool = new PGlitePool();
    try {
      const store = createPostgresDurabilityStore(pool.asPostgresPool());
      const owner = await store.acquire("failures", { ownerId: "failure-owner" });
      pool.failNextBefore(/^BEGIN$/);
      await expect(store.replace("failures", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: durabilityRevision("0"),
        payload: "never-started",
      })).rejects.toThrow("injected query failure");

      pool.failNextAfter(/^INSERT INTO nanocodex_durable_states/);
      await expect(store.replace("failures", {
        ownerId: owner.ownerId,
        fence: owner.fence,
        expectedRevision: durabilityRevision("0"),
        payload: "rolled-back",
      })).resolves.toEqual({ status: "not_committed", message: "injected query failure" });
      await expect(store.load("failures")).resolves.toEqual({
        revision: durabilityRevision("0"),
        payload: null,
      });
    } finally {
      await pool.close();
    }
  });

  it("round-trips one compacted multi-turn Agent through Cloudflare and PostgreSQL", async () => {
    const module = await readFile(
      new URL("../../../js/nanocodex/pkg-web/nanocodex_bg.wasm", import.meta.url),
    );
    const server = await startResponsesServer();
    const pool = new PGlitePool();
    const source = createCloudflareDurabilityStore(cloudflareDurabilityStorage());
    const postgres = createPostgresDurabilityStore(pool.asPostgresPool());
    const returned = createCloudflareDurabilityStore(cloudflareDurabilityStorage());
    const cloudflareSessionId = "018f1f9a-7b3c-7a70-8000-000000000070";
    const vercelSessionId = "018f1f9a-7b3c-7a70-8000-000000000071";
    const returnedSessionId = "018f1f9a-7b3c-7a70-8000-000000000072";
    const stateId = "portable-managed-agent";
    let agent: NodeAgent | undefined;

    try {
      agent = await Agent.create({
        module,
        sessionId: cloudflareSessionId,
        thinking: "none",
        transport: Transport.openAi({ apiKey: "test-key", websocketUrl: server.url }),
        durability: source,
        durabilityId: stateId,
      });
      const sourceScenario = (async () => {
        const socket = await server.nextConnection();
        await answerMessage(socket, "cloudflare-response-1", "CLOUDFLARE TURN ONE", {
          inputs: ["first Cloudflare turn"],
        });
        await answerMessage(socket, "cloudflare-response-2", "CLOUDFLARE TURN TWO", {
          inputs: ["second Cloudflare turn"],
          previousResponseId: "cloudflare-response-1",
        });
        await answerCompaction(socket, "cloudflare-compaction", {
          previousResponseId: "cloudflare-response-2",
        });
        await answerMessage(socket, "cloudflare-response-3", "CLOUDFLARE COMPACTED", {
          inputs: ["opaque-portable-summary", "source after compaction"],
        });
      })();
      await expect(runTurn(agent, "turn-cloudflare-1", "first Cloudflare turn"))
        .resolves.toBe("CLOUDFLARE TURN ONE");
      await expect(runTurn(agent, "turn-cloudflare-2", "second Cloudflare turn"))
        .resolves.toBe("CLOUDFLARE TURN TWO");
      await agent.session.compact();
      await expect(runTurn(agent, "turn-cloudflare-3", "source after compaction"))
        .resolves.toBe("CLOUDFLARE COMPACTED");
      await sourceScenario;
      await shutdownAgent(agent);
      agent = undefined;

      const cloudflarePages = await exportPages(source, stateId, durabilityRevision("0"));
      const cloudflareTo = cloudflarePages[0]!.to;
      const importedPostgresState = await importDurabilityStatePages(
        postgres,
        jsonRoundTrip(cloudflarePages),
      );
      expect(importedPostgresState).toEqual({
        revision: cloudflareTo,
        payload: cloudflarePages.map((page) => page.payload).join(""),
      });
      const cloudflareDigest = await durabilityStateDigest(importedPostgresState);

      agent = await Agent.create({
        module,
        sessionId: vercelSessionId,
        thinking: "none",
        transport: Transport.openAi({ apiKey: "test-key", websocketUrl: server.url }),
        durability: postgres,
        durabilityId: stateId,
      });
      await expect(runTurn(agent, "turn-cloudflare-1", "first Cloudflare turn"))
        .resolves.toBe("CLOUDFLARE TURN ONE");
      await expect(runTurn(agent, "turn-cloudflare-2", "second Cloudflare turn"))
        .resolves.toBe("CLOUDFLARE TURN TWO");
      await expect(runTurn(agent, "turn-cloudflare-3", "source after compaction"))
        .resolves.toBe("CLOUDFLARE COMPACTED");
      expect(server.connections).toBe(1);

      const postgresScenario = (async () => {
        const socket = await server.nextConnection();
        await answerMessage(socket, "postgres-response-1", "POSTGRES TURN ONE", {
          inputs: [
            "opaque-portable-summary",
            "source after compaction",
            "CLOUDFLARE COMPACTED",
            "first Vercel Postgres turn",
          ],
        });
        await answerMessage(socket, "postgres-response-2", "POSTGRES TURN TWO", {
          inputs: ["second Vercel Postgres turn"],
          previousResponseId: "postgres-response-1",
        });
      })();
      await expect(runTurn(agent, "turn-postgres-1", "first Vercel Postgres turn"))
        .resolves.toBe("POSTGRES TURN ONE");
      await expect(runTurn(agent, "turn-postgres-2", "second Vercel Postgres turn"))
        .resolves.toBe("POSTGRES TURN TWO");
      await postgresScenario;
      expect(server.connections).toBe(2);
      await shutdownAgent(agent);
      agent = undefined;

      const postgresPages = await exportPages(postgres, stateId, cloudflareTo, cloudflareDigest);
      const postgresTo = postgresPages[0]!.to;
      expect(BigInt(postgresTo)).toBeGreaterThan(BigInt(cloudflareTo));
      expect(postgresPages.map((page) => page.payload).join(""))
        .toContain("opaque-portable-summary");
      await importDurabilityStatePages(returned, jsonRoundTrip(cloudflarePages));
      await importDurabilityStatePages(returned, jsonRoundTrip(postgresPages));

      agent = await Agent.create({
        module,
        sessionId: returnedSessionId,
        thinking: "none",
        transport: Transport.openAi({ apiKey: "test-key", websocketUrl: server.url }),
        durability: returned,
        durabilityId: stateId,
      });
      const replayedTurns = [
        ["turn-cloudflare-1", "first Cloudflare turn", "CLOUDFLARE TURN ONE"],
        ["turn-cloudflare-2", "second Cloudflare turn", "CLOUDFLARE TURN TWO"],
        ["turn-cloudflare-3", "source after compaction", "CLOUDFLARE COMPACTED"],
        ["turn-postgres-1", "first Vercel Postgres turn", "POSTGRES TURN ONE"],
        ["turn-postgres-2", "second Vercel Postgres turn", "POSTGRES TURN TWO"],
      ] as const;
      for (const [id, input, expected] of replayedTurns) {
        await expect(runTurn(agent, id, input)).resolves.toBe(expected);
      }
      expect(server.connections).toBe(2);

      const returnScenario = (async () => {
        const socket = await server.nextConnection();
        await answerMessage(socket, "returned-response-1", "RETURNED TURN ONE", {
          inputs: [
            "opaque-portable-summary",
            "POSTGRES TURN ONE",
            "second Vercel Postgres turn",
            "POSTGRES TURN TWO",
            "first returned Cloudflare turn",
          ],
        });
        await answerMessage(socket, "returned-response-2", "RETURNED TURN TWO", {
          inputs: ["second returned Cloudflare turn"],
          previousResponseId: "returned-response-1",
        });
      })();
      await expect(runTurn(agent, "turn-returned-1", "first returned Cloudflare turn"))
        .resolves.toBe("RETURNED TURN ONE");
      await expect(runTurn(agent, "turn-returned-2", "second returned Cloudflare turn"))
        .resolves.toBe("RETURNED TURN TWO");
      expect(server.connections).toBe(3);
      await returnScenario;
      expect(BigInt((await returned.load(stateId)).revision)).toBeGreaterThan(
        BigInt(postgresTo),
      );
    } finally {
      try {
        if (agent) await shutdownAgent(agent);
      } finally {
        try {
          await server.close();
        } finally {
          await pool.close();
        }
      }
    }
  }, 30_000);
});

async function exportPages(
  store: Parameters<typeof exportDurabilityStatePage>[0],
  stateId: string,
  from: ReturnType<typeof durabilityRevision>,
  fromDigest?: string,
): Promise<DurabilityPortableStatePage[]> {
  const pages: DurabilityPortableStatePage[] = [];
  let cursor: string | undefined;
  let to: ReturnType<typeof durabilityRevision> | undefined;
  do {
    const page = await exportDurabilityStatePage(store, stateId, {
      from,
      ...(fromDigest === undefined ? {} : { fromDigest }),
      to,
      cursor,
      limit: 97,
    });
    pages.push(page);
    to = page.to;
    cursor = page.nextCursor ?? undefined;
  } while (cursor !== undefined);
  return pages;
}

function jsonRoundTrip(pages: DurabilityPortableStatePage[]): DurabilityPortableStatePage[] {
  return JSON.parse(JSON.stringify(pages)) as DurabilityPortableStatePage[];
}

async function shutdownAgent(agent: NodeAgent): Promise<void> {
  try {
    await agent.session.shutdown();
  } finally {
    agent.dispose();
  }
}

async function runTurn(
  agent: NodeAgent,
  id: string,
  input: string,
): Promise<string | undefined> {
  const turn = agent.turn.prompt({ id, input });
  try {
    const result = await turn.result();
    try {
      return result.finalMessage;
    } finally {
      result.dispose();
    }
  } finally {
    turn.dispose();
  }
}

async function answerMessage(
  socket: WebSocket,
  responseId: string,
  message: string,
  expected: Readonly<{ inputs: readonly string[]; previousResponseId?: string }>,
): Promise<void> {
  const request = await nextMessage(socket);
  socket.send(JSON.stringify({
    type: "response.completed",
    response: {
      id: responseId,
      status: "completed",
      output: [{
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: message }],
      }],
      usage: null,
    },
  }));
  const input = JSON.stringify(request.input);
  for (const text of expected.inputs) expect(input).toContain(text);
  expect(request.previous_response_id).toBe(expected.previousResponseId);
}

async function answerCompaction(
  socket: WebSocket,
  responseId: string,
  expected: Readonly<{ previousResponseId?: string }>,
): Promise<void> {
  const request = await nextMessage(socket);
  expect(request.input).toEqual([{ type: "compaction_trigger" }]);
  expect(request.previous_response_id).toBe(expected.previousResponseId);
  socket.send(JSON.stringify({
    type: "response.output_item.done",
    item: {
      id: "portable-compaction-item",
      type: "compaction",
      encrypted_content: "opaque-portable-summary",
    },
  }));
  socket.send(JSON.stringify({
    type: "response.completed",
    response: {
      id: responseId,
      status: "completed",
      output: [],
      usage: null,
    },
  }));
}

type ResponsesServer = Readonly<{
  connections: number;
  url: string;
  nextConnection(): Promise<WebSocket>;
  close(): Promise<void>;
}>;

async function startResponsesServer(): Promise<ResponsesServer> {
  const websocketServer = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise<void>((resolve, reject) => {
    websocketServer.once("listening", resolve);
    websocketServer.once("error", reject);
  });
  const queued: WebSocket[] = [];
  const waiters: Array<(socket: WebSocket) => void> = [];
  let connections = 0;
  websocketServer.on("connection", (socket) => {
    connections += 1;
    const resolve = waiters.shift();
    if (resolve) resolve(socket);
    else queued.push(socket);
  });
  return Object.freeze({
    get connections() { return connections; },
    get url() {
      const address = websocketServer.address();
      if (!address || typeof address === "string") throw new Error("Responses server is not listening");
      return `ws://127.0.0.1:${address.port}`;
    },
    nextConnection() {
      const socket = queued.shift();
      return socket ? Promise.resolve(socket) : new Promise((resolve) => waiters.push(resolve));
    },
    close() {
      for (const socket of websocketServer.clients) socket.terminate();
      return new Promise<void>((resolve, reject) => {
        websocketServer.close((error) => error ? reject(error) : resolve());
      });
    },
  });
}

function nextMessage(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve) => {
    socket.once("message", (data: RawData) => {
      resolve(JSON.parse(data.toString("utf8")) as Record<string, unknown>);
    });
  });
}

type InjectedFailure = { pattern: RegExp; timing: "before" | "after" };

class PGlitePool {
  readonly #database = new PGlite();
  readonly #failCommitAfter: number | undefined;
  #commits = 0;
  #connectionTail = Promise.resolve();
  #failure: InjectedFailure | undefined;
  readonly clientQueries: string[] = [];
  readonly releases: boolean[] = [];

  constructor(options: { failCommitAfter?: number } = {}) {
    this.#failCommitAfter = options.failCommitAfter;
  }

  asPostgresPool(): PostgresDurabilityPool { return this; }
  failNextBefore(pattern: RegExp): void { this.#failure = { pattern, timing: "before" }; }
  failNextAfter(pattern: RegExp): void { this.#failure = { pattern, timing: "after" }; }

  async query<Row extends PostgresDurabilityRow = PostgresDurabilityRow>(
    text: string,
    values: unknown[] = [],
  ): Promise<PostgresDurabilityQueryResult<Row>> {
    const result = await this.#database.query<Row>(text, [...values]);
    return { rows: result.rows };
  }

  async connect(): Promise<PostgresDurabilityClient> {
    const previous = this.#connectionTail;
    let unlock!: () => void;
    this.#connectionTail = new Promise<void>((resolve) => { unlock = resolve; });
    await previous;
    let released = false;
    return {
      query: async <Row extends PostgresDurabilityRow = PostgresDurabilityRow>(
        text: string,
        values?: unknown[],
      ) => {
        const query = text.trim();
        this.clientQueries.push(query);
        if (this.#takeFailure(query, "before")) throw new Error("injected query failure");
        const result = await this.query<Row>(text, values);
        if (text === "COMMIT") {
          this.#commits += 1;
          if (this.#commits === this.#failCommitAfter) {
            throw new Error("connection disappeared after COMMIT was applied");
          }
        }
        if (this.#takeFailure(query, "after")) throw new Error("injected query failure");
        return result;
      },
      release: (discard?: Error | boolean) => {
        if (released) return;
        released = true;
        this.releases.push(discard === true || discard instanceof Error);
        unlock();
      },
    };
  }

  #takeFailure(query: string, timing: InjectedFailure["timing"]): boolean {
    const failure = this.#failure;
    if (!failure || failure.timing !== timing || !failure.pattern.test(query)) return false;
    this.#failure = undefined;
    return true;
  }

  async close(): Promise<void> {
    await this.#connectionTail;
    await this.#database.close();
  }
}
