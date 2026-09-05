import assert from "node:assert/strict";
import test from "node:test";
import {
  listManagedConversations,
  loadManagedConversationSelection,
} from "./managedAgentRuntime.ts";

const FIRST_AGENT_ID = "018f0000-0000-7000-8000-000000000001";
const SECOND_AGENT_ID = "018f0000-0000-7000-8000-000000000002";
const FORBIDDEN_AGENT_ID = "018f0000-0000-7000-8000-000000000003";

test("an exact agent route survives a successful list cached before another client created it", async (t) => {
  const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const originalFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL("https://account.example"),
  });
  let agentIds = [FIRST_AGENT_ID];
  let listCalls = 0;
  const exactCalls: string[] = [];
  globalThis.fetch = async (input, init) => {
    const request = new Request(input, init);
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path === "/v1/agents") {
      listCalls += 1;
      return Response.json({
        data: agentIds,
        summaries: Object.fromEntries(agentIds.map((id, index) => [id, {
          title: `Agent ${index + 1}`,
          created_at: index + 1,
          updated_at: agentIds.length - index,
          turn_count: index,
        }])),
      });
    }
    const exactId = decodeURIComponent(path.slice("/v1/agents/".length));
    exactCalls.push(exactId);
    if (request.method === "GET" && agentIds.includes(exactId)) return Response.json({});
    return Response.json(
      { error: "forbidden", message: "That exact agent is not available to this account." },
      { status: 403 },
    );
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
    if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
    else Reflect.deleteProperty(globalThis, "location");
  });

  const initial = await listManagedConversations("stale-list-client");
  assert.deepEqual(initial.map(({ id }) => id), [FIRST_AGENT_ID]);
  agentIds = [SECOND_AGENT_ID, FIRST_AGENT_ID];

  const stale = await listManagedConversations("stale-list-client");
  assert.deepEqual(stale.map(({ id }) => id), [FIRST_AGENT_ID]);
  assert.equal(listCalls, 1);

  const routed = await loadManagedConversationSelection({
    accountId: "stale-list-client",
    routeAgentId: SECOND_AGENT_ID,
    retainedAgentId: FIRST_AGENT_ID,
    hasCredential: true,
  });
  assert.equal(routed.selectedId, SECOND_AGENT_ID);
  assert.equal(routed.replaceRoute, false);
  assert.deepEqual(routed.conversations.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.deepEqual(exactCalls, [SECOND_AGENT_ID]);
  assert.equal(listCalls, 1);

  const augmented = await listManagedConversations("stale-list-client");
  assert.deepEqual(augmented.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.equal(listCalls, 1);

  const refreshed = await listManagedConversations("stale-list-client", { refresh: true });
  assert.deepEqual(refreshed.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.equal(listCalls, 2);

  await assert.rejects(
    loadManagedConversationSelection({
      accountId: "stale-list-client",
      routeAgentId: FORBIDDEN_AGENT_ID,
      retainedAgentId: FIRST_AGENT_ID,
      hasCredential: true,
    }),
    /That exact agent is not available to this account/,
  );
  assert.deepEqual(exactCalls, [SECOND_AGENT_ID, FORBIDDEN_AGENT_ID]);
});

test("an empty account creates its first durable conversation with Astra settings", async (t) => {
  const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const originalFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL("https://account.example"),
  });
  let creationBody: unknown;
  globalThis.fetch = async (input, init) => {
    const request = new Request(input, init);
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path === "/v1/agents") {
      return Response.json({ data: [], summaries: {} });
    }
    if (request.method === "POST" && path === "/v1/agents") {
      creationBody = await request.json();
      return Response.json({ agent_id: FIRST_AGENT_ID, session_id: FIRST_AGENT_ID });
    }
    throw new Error(`unexpected request: ${request.method} ${path}`);
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
    if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
    else Reflect.deleteProperty(globalThis, "location");
  });

  const selected = await loadManagedConversationSelection({
    accountId: "astra-empty-account",
    hasCredential: true,
  });

  assert.equal(selected.selectedId, FIRST_AGENT_ID);
  assert.deepEqual(creationBody, {
    settings: {
      model: "gpt-6-astra",
      thinking: "high",
      reasoning_mode: "standard",
      fast_mode: false,
    },
  });
});
