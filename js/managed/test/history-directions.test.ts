import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DurableEventLog } from "../src/durable-events";
import { ManagedEventArchive } from "../src/managed-event-archive";
import type { DurableAgentSession } from "../src/index";

type Page = { data: { cursor: string }[]; latest_cursor: string; has_more: boolean };

it("pages complete long histories in either direction across the archive boundary with distinct private validators", async () => {
  const runtime = env as unknown as {
    NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    NANOCODEX_HISTORY: R2Bucket;
  };
  await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(crypto.randomUUID()), async (session, state) => {
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
       public_origin, runtime_profile, last_active)
      VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', ?)`,
    crypto.randomUUID(), owner, organization, team, Date.now());
    const headers = {
      "x-nanocodex-owner-id": owner,
      "x-nanocodex-session-organization-id": organization,
      "x-nanocodex-session-team-id": team,
      "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": '["agents:write","tools:use"]',
    };
    const log = new DurableEventLog<{ type: string; text: string }>(state.storage);
    const archive = new ManagedEventArchive(state.storage, runtime.NANOCODEX_HISTORY, state.id.toString(), {
      recentEventCount: 64, segmentTargetBytes: 32 * 1024, sealThresholdBytes: 1,
    });
    const total = 2305;
    for (let i = 0; i < total; i++) log.append({ type: "message", text: `message ${i}` });
    const fetchPage = (query: string, extra: Record<string, string> = {}) => session.fetch(new Request(
      `https://session.internal/events/history?limit=128&${query}`, { headers: { ...headers, ...extra } },
    ));
    try {
      while ((await archive.seal(true)).sealed) { /* retain a local tail */ }
      expect(Number(archive.archivedThrough())).toBeGreaterThan(2048);
      expect(Number(archive.archivedThrough())).toBeLessThan(total);
      const expected = Array.from({ length: total }, (_, i) => String(i + 1));
      for (const direction of ["after", "before"] as const) {
        let cursor: string | undefined = direction === "after" ? "0" : undefined;
        const received: string[] = [];
        for (let requests = 0; ; requests++) {
          expect(requests).toBeLessThan(total);
          const response = await fetchPage(cursor === undefined ? "" : `${direction}=${cursor}`);
          expect(response.status).toBe(200);
          const page = await response.json<Page>();
          expect(page.latest_cursor).toBe(String(total));
          expect(page.data.length).toBeGreaterThan(0);
          expect(page.data.length).toBeLessThanOrEqual(128);
          const cursors = page.data.map((event) => event.cursor);
          expect(cursors).toEqual([...cursors].sort((a, b) => Number(a) - Number(b)));
          if (direction === "after") received.push(...cursors);
          else received.unshift(...cursors);
          if (!page.has_more) break;
          cursor = direction === "after" ? cursors.at(-1)! : cursors[0]!;
        }
        expect(received).toEqual(expected);
      }
      for (const query of ["after=2305", "after=3000", "before=1"]) {
        const page = await (await fetchPage(query)).json<Page>();
        expect(page.data).toEqual([]); expect(page.has_more).toBe(false);
      }
      let previousTag = "";
      for (const query of ["after=1152", "before=1152"]) {
        const response = await fetchPage(query, { "if-none-match": previousTag });
        expect(response.status).toBe(200);
        const etag = response.headers.get("etag")!;
        expect(etag).not.toBe(previousTag);
        expect(response.headers.get("cache-control")).toBe("private, no-cache");
        expect(response.headers.get("vary")).toBe("Authorization, Cookie");
        await response.json();
        expect((await fetchPage(query, { "if-none-match": etag })).status).toBe(304);
        expect((await fetchPage(query, { "if-none-match": etag, "x-nanocodex-owner-id": crypto.randomUUID() })).status).toBe(404);
        previousTag = etag;
      }
      for (const query of ["before=10&after=0", "after=-1", "after=", "after=9223372036854775808"]) {
        expect((await fetchPage(query)).status).toBe(400);
      }
      const old = await fetchPage("after=2305");
      const tag = old.headers.get("etag")!; await old.json();
      log.append({ type: "message", text: "appended after the cached boundary" });
      const updated = await fetchPage("after=2305", { "if-none-match": tag });
      expect(updated.status).toBe(200);
      expect(await updated.json()).toMatchObject({ data: [{ cursor: "2306" }], has_more: false });
    } finally { await archive.deleteAll(); log.clear(); }
  });
});
