import { applyD1Migrations, env, runInDurableObject } from "cloudflare:test";
import { beforeAll, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";

beforeAll(async () => {
  const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
  await applyD1Migrations(bindings.NANOCODEX_CRM, bindings.CRM_MIGRATIONS);
});

// Exercise tool discovery, Code Mode, retained turn authority, and D1 through three
// real managed sessions; deterministic provider replies keep this reproducible.
it("imports and researches a meeting, recalls missing notes, and records the user’s notes across conversations", async () => {
  const { crmRequest } = await import("../src/crm");
  const db = (env as unknown as { NANOCODEX_CRM: D1Database }).NANOCODEX_CRM;
  const principal: Principal = {
    kind: "api_key", userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: "user:crm-fixture", credentialId: "crm-fixture", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"],
  };
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  const connectionId = "c".repeat(43);
  const ended = new Date(Date.now() - 3_600_000).toISOString();
  const event = { id: "workshop-meeting", status: "confirmed", summary: "Jamie / Owner", updated: ended,
    start: { dateTime: new Date(Date.now() - 7_200_000).toISOString() }, end: { dateTime: ended },
    description: "Intro about database engineering", organizer: { self: true, email: "owner@example.test" },
    attendees: [{ self: true, email: "owner@example.test", responseStatus: "accepted" }, { email: "jamie@example.test", displayName: "Jamie Example", responseStatus: "accepted" }] };
  let calendarReads = 0, emailReads = 0;
  const codes = [
    `await tools.crm_sync({connection_id:"${connectionId}"}); const person=(await tools.crm_research({operation:"queue"})).records[0]; const mail=await tools.gmail_request({connection_id:"${connectionId}",path:"/gmail/v1/users/me/messages/001122aabbccddff?format=full"}); await tools.crm_research({operation:"save",record_id:person.id,status:"complete",summary:mail.data.snippet,company:"Fixture Research",sources:[{kind:"email",reference:"001122aabbccddff"},{kind:"calendar",reference:"workshop-meeting"}]}); await tools.crm_facts({operation:"save",record_id:person.id,predicate:"bio.expertise",value:["database engineering"],origin:"source",sources:[{kind:"email",reference:"001122aabbccddff"}]}); const company=(await tools.crm_save({kind:"company",name:"Fixture Research"})).record; await tools.crm_relationships({operation:"save",from_id:person.id,to_id:company.id,type:"works_at",role:"Engineer",origin:"source",sources:[{kind:"email",reference:"001122aabbccddff"}]}); text(await tools.crm_meetings({operation:"list",needs_notes:true}));`,
    'const meeting=(await tools.crm_meetings({operation:"list",needs_notes:true})).meetings[0]; text(await tools.crm_meetings({operation:"get",id:meeting.id})); const person=(await tools.crm_search({q:"Fixture Research",kind:"person"})).records[0]; text(await tools.crm_get({id:person.id}));',
    'const meeting=(await tools.crm_meetings({operation:"list",needs_notes:true})).meetings[0]; await tools.crm_meetings({operation:"note",meeting_id:meeting.id,body:"We discussed database benchmarks; I will send our results."}); text(await tools.crm_meetings({operation:"list",needs_notes:true}));',
  ];
  for (const [journey, code] of codes.entries()) {
    const id = crypto.randomUUID(), turnId = crypto.randomUUID();
    await runInDurableObject(sessions.getByName(id), async (session, state) => {
      const transcript: unknown[] = [];
      let step = 0;
      class ModelSocket extends EventTarget {
        readyState = 1; bufferedAmount = 0;
        accept() {}
        close() { this.readyState = 3; }
        send(value: string) {
          transcript.push(JSON.parse(value));
          const output = step++ === 0
            ? [{ type: "custom_tool_call", name: "exec", call_id: "crm-journey", input: code }]
            : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "DONE" }] }];
          queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
            type: "response.completed", response: { id: `crm-${step}`, status: "completed", output,
              usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 } },
          }) })));
        }
      }
      const original = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { configurable: true, value: { ...original,
        NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
          const url = new URL(input instanceof Request ? input.url : String(input));
          if (url.hostname === "www.googleapis.com" && url.pathname.endsWith("/events")) { calendarReads++; return Response.json({ items: [event] }); }
          if (url.hostname === "gmail.googleapis.com") { emailReads++; return Response.json({ id: "001122aabbccddff", snippet: "Jamie is an engineer at Fixture Research, introduced for database work." }); }
          if (url.pathname.includes("/responses")) return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
          if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
          return Response.json({ connectors: {}, mcp_connections: [], vault: [] });
        } },
        NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [] }) }) },
      } });
      const call = (path: string, body: unknown) => {
        const headers = new Headers({ "content-type": "application/json" });
        if (path !== "/create") forwardPrincipalAssertions(headers, principal);
        return session.fetch(new Request(`https://session.internal${path}`, { method: "POST", headers, body: JSON.stringify(body) }));
      };
      expect((await call("/create", { session_id: id, owner_id: principal.userId, organization_id: principal.organizationId,
        team_id: principal.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
        configuration: {} })).status).toBe(200);
      try {
        expect((await call("/turns", { id: turnId, input: ["Sync my calendar and research the people I met.", "Which meetings are still missing my notes?", "Add to Jamie’s meeting: we discussed database benchmarks; I will send our results."][journey] })).status).toBe(202);
        await expect.poll(() => state.storage.sql.exec("SELECT state,error FROM managed_turns WHERE id=?", turnId).one(), { timeout: 20_000 })
          .toEqual({ state: "completed", error: null });
        console.info(JSON.stringify({ scenario: "crm.managed", journey, turn: turnId, model_requests: transcript.length,
          last_model_input: (transcript.at(-1) as { input?: unknown }).input }));
        const stored = await crmRequest(db, principal.userId, "search", { q: "Jamie Example", kind: "person" }, "unused") as { records: { name: string }[] };
        expect(stored.records.map(r => r.name)).toEqual(["Jamie Example"]);
        expect(transcript.length).toBe(2);
        const { crmMeetingRequest } = await import("../src/crm-meetings");
        const missing = await crmMeetingRequest(db, principal.userId, "list", { needs_notes: true }, "unused") as { meetings: unknown[] };
        expect(missing.meetings).toHaveLength(journey === 2 ? 0 : 1);
        if (journey === 1) {
          expect(JSON.stringify(transcript.at(-1))).toContain("Jamie is an engineer at Fixture Research");
          expect(JSON.stringify(transcript.at(-1))).toContain("bio.expertise");
          expect(JSON.stringify(transcript.at(-1))).toContain("works_at");
        }
      } finally {
        state.storage.sql.exec("UPDATE session_state SET last_active=0");
        await session.alarm();
        await state.storage.deleteAlarm();
      }
    });
  }
  expect(calendarReads).toBe(1);
  expect(emailReads).toBe(1);
}, 60_000);
