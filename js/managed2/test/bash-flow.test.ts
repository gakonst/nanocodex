import { env, evictDurableObject, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

// Failure modes: VFS writes lost after eviction, one agent reading another's
// files, shell writes escaping /brain, and a chat-only turn loading Bash.
it("runs Just Bash through a real Worker/model/tool continuation with per-agent durable SQLite", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  async function create() {
    const response = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization } });
    expect(response.status).toBe(201);
    return (await response.json<{ agent_id: string }>()).agent_id;
  }
  async function execute(agent: string, cmd: string) {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent}/turns`, {
      method: "POST", headers: { authorization }, body: JSON.stringify({ input: `Use exec_command: ${cmd}` }),
    });
    expect(response.status).toBe(202);
    const { turn_id } = await response.json<{ turn_id: string }>();
    let status: { state: string; message?: string; timing?: { trace_id: string; tool_calls: number } } | undefined;
    await expect.poll(async () => {
      status = await (await SELF.fetch(`https://api.test/v1/agents/${agent}/turns/${turn_id}`, { headers: { authorization } })).json();
      return status?.state;
    }, { timeout: 20_000 }).toBe("completed");
    expect(status?.timing?.tool_calls).toBe(1);
    return status!.message!;
  }
  const first = await create();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS.getByName(`fixture-user:${first}`);
  const tableExists = () => runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{ name: string }>(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'bash_files'",
  ).toArray().length !== 0);
  expect(await tableExists()).toBe(false);
  const chat = await SELF.fetch(`https://api.test/v1/agents/${first}/turns`, {
    method: "POST", headers: { authorization }, body: JSON.stringify({ input: "Say hello" }),
  });
  const { turn_id: chatId } = await chat.json<{ turn_id: string }>();
  await expect.poll(async () => (await (await SELF.fetch(`https://api.test/v1/agents/${first}/turns/${chatId}`, {
    headers: { authorization },
  })).json<{ state: string }>()).state).toBe("completed");
  expect(await tableExists()).toBe(false);
  expect(await execute(first, "mkdir -p /brain/notes && printf persisted > /brain/notes/item")).toContain('"exit_code":0');
  expect(await tableExists()).toBe(true);
  await evictDurableObject(stub);
  expect(await execute(first, "cat /brain/notes/item")).toContain("persisted");
  const other = await create();
  expect(await execute(other, "cat /brain/notes/item")).toContain('"exit_code":1');
  expect(await execute(first, "printf outside > /outside")).not.toContain('"exit_code":0');
}, 20_000);
