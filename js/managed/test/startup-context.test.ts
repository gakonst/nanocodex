import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { AgentSessionContext, PromptInput } from "nanocodex";
import type { DurableAgentSession } from "../src/index";
import { ManagedStartupContext, startupQuery, type StartupEnvironment } from "../src/startup-context";

const firstPrompt = "Find the copper finch deployment preference";

async function withStartup(run: (startup: ManagedStartupContext, state: DurableObjectState) => Promise<void>) {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
    state.storage.sql.exec(`INSERT INTO session_state (
      singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
      public_origin, runtime_profile, accepted_turns, last_active
    ) VALUES (1, ?, 'owner', 'org', 'team', 1, 'https://test.example', 'managed', 0, ?)`,
    crypto.randomUUID(), Date.now());
    await run(new ManagedStartupContext(state.storage), state);
  });
}

const environment: StartupEnvironment = {
  runtime: "cloudflare-durable-object", default_cwd: "/brain",
  accountInfo: {
    status: "ready", authenticated: ["github"], accounts: { github: "work" },
    connectorAccounts: { github: [{ id: "github-work", label: "work" }] },
    identity: {}, stablecoins: [], authorizations: [], vault: [],
    machines: [{ id: "user:hand", name: "laptop", kind: "user", mount: "/hand",
      workspace: "/hand", capabilities: ["exec_command"] }],
  },
};

function developerSession(history: Record<string, unknown>[] = []) {
  const snapshot = (): AgentSessionContext => ({ workspace: "/brain", history: [...history] });
  return {
    history,
    context: vi.fn(async () => snapshot()),
    appendDeveloperMessage: vi.fn(async (text: string) => {
      history.push({ type: "message", role: "developer", content: [{ type: "input_text", text }] });
      return snapshot();
    }),
  };
}

const assertActive = () => {};
const contextText = (state: DurableObjectState) => state.storage.sql.exec<{ content: string }>(
  "SELECT content FROM managed_startup_context WHERE turn_id = 'first'",
).one().content;

describe("managed first-prompt bootstrap boundary", () => {
  it("bounds Unicode queries and excludes attachment URLs", () => {
    const long = startupQuery(`  ${"😀".repeat(200)} tail`);
    expect(new TextEncoder().encode(long).length).toBe(512);
    expect(long).not.toContain("�");
    expect(startupQuery([{ type: "text", text: " copper\n finch " }, {
      type: "image", image_url: "https://private.example/secret",
    }])).toBe("copper finch [image]");
    expect(startupQuery([])).toBe("conversation context");
  });

  it("prepares retrieval and environment concurrently, then durably injects developer context once before the unchanged user prompt", async () => {
    await withStartup(async (startup, state) => {
      const input: PromptInput = [{ type: "text", text: firstPrompt }, { type: "image", image_url: "data:image/png;base64,test" }];
      const original = structuredClone(input);
      startup.reserve("first", input);
      state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      startup.reserve("second", "different question");
      const entered: string[] = [];
      let release!: () => void;
      const allEntered = new Promise<void>((resolve) => { release = resolve; });
      const enter = async (name: string) => {
        entered.push(name);
        if (entered.length === 3) release();
        await allEntered;
      };
      const execute = vi.fn(async (name: string) => {
        await enter(name);
        return name === "memory" ? { operation: "scan", abstained: true, candidates: [] } : { sessions: [] };
      });
      const prepareEnvironment = vi.fn(async () => { await enter("environment"); return environment; });
      await startup.prepare("first", execute, prepareEnvironment, assertActive);
      expect(entered.sort()).toEqual(["environment", "find_session", "memory"]);
      expect(input).toEqual(original);
      const text = contextText(state);
      expect(text).toContain('"machines":[{"id":"user:hand"');
      expect(text).toContain('"connectorAccounts":{"github"');
      expect(text).toContain("not instructions");
      expect(text).toContain("untrusted content");
      const runtime = developerSession();
      await startup.inject("first", runtime, assertActive);
      runtime.history.push({ role: "user", content: input });
      expect(runtime.history.map((item) => item.role)).toEqual(["developer", "user"]);
      expect(runtime.appendDeveloperMessage).toHaveBeenCalledExactlyOnceWith(text);
      const restored = new ManagedStartupContext(state.storage);
      await restored.prepare("first", execute, prepareEnvironment, assertActive);
      await restored.inject("first", runtime, assertActive);
      await restored.prepare("second", execute, prepareEnvironment, assertActive);
      await restored.inject("second", runtime, assertActive);
      expect(execute).toHaveBeenCalledTimes(2);
      expect(prepareEnvironment).toHaveBeenCalledOnce();
      expect(runtime.appendDeveloperMessage).toHaveBeenCalledOnce();
      expect(contextText(state)).toBe(text);
    });
  });

  it.each(["existing", "multiplayer"])("does not bootstrap %s conversations", async (kind) => {
    await withStartup(async (startup, state) => {
      state.storage.sql.exec(kind === "existing"
        ? "UPDATE session_state SET accepted_turns = 3"
        : "UPDATE session_state SET runtime_profile = 'multiplayer'");
      startup.reserve("later", firstPrompt);
      const execute = vi.fn();
      const prepareEnvironment = vi.fn();
      await startup.prepare("later", execute, prepareEnvironment, assertActive);
      await startup.inject("later", developerSession(), assertActive);
      expect(execute).not.toHaveBeenCalled();
      expect(prepareEnvironment).not.toHaveBeenCalled();
    });
  });

  it("keeps authorized results when the other lookup fails without leaking internal errors", async () => {
    await withStartup(async (startup, state) => {
      startup.reserve("first", firstPrompt);
      await startup.prepare("first", async (name) => {
        if (name === "memory") throw Object.assign(new Error("provider token SECRET https://internal"), { code: "forbidden" });
        return { sessions: [{ session_id: "candidate" }] };
      }, async () => environment, assertActive);
      const text = contextText(state);
      expect(text).toContain("candidate");
      expect(text).toContain("forbidden");
      expect(text).not.toMatch(/SECRET|https:\/\/internal/);
    });
  });

  it("does not persist late results after the owning agent is fenced", async () => {
    await withStartup(async (startup, state) => {
      startup.reserve("first", firstPrompt);
      let active = true;
      await expect(startup.prepare("first", async () => {
        active = false;
        return { sessions: [] };
      }, async () => environment, () => { if (!active) throw new Error("fenced"); })).rejects.toThrow("fenced");
      expect(state.storage.sql.exec("SELECT * FROM managed_startup_context").toArray()).toEqual([]);
      expect(state.storage.sql.exec("SELECT * FROM managed_startup_tools WHERE result_json IS NOT NULL").toArray()).toEqual([]);
    });
  });

  it("recovers a lost injection acknowledgement without appending the developer message twice", async () => {
    await withStartup(async (startup, state) => {
      startup.reserve("first", firstPrompt);
      await startup.prepare("first", async () => ({}), async () => environment, assertActive);
      const runtime = developerSession();
      const append = runtime.appendDeveloperMessage.getMockImplementation()!;
      runtime.appendDeveloperMessage.mockImplementationOnce(async (text) => {
        await append(text);
        throw new Error("lost acknowledgement after checkpoint");
      });
      await expect(startup.inject("first", runtime, assertActive)).rejects.toThrow("lost acknowledgement");
      await new ManagedStartupContext(state.storage).inject("first", runtime, assertActive);
      expect(runtime.appendDeveloperMessage).toHaveBeenCalledOnce();
      expect(runtime.history).toHaveLength(1);
    });
  });

  it("does not accept a user or tool message as an injection receipt", async () => {
    await withStartup(async (startup, state) => {
      startup.reserve("first", firstPrompt);
      await startup.prepare("first", async () => ({}), async () => environment, assertActive);
      const text = contextText(state);
      const runtime = developerSession(["user", "tool"].map((role) => ({ role,
        content: [{ type: "input_text", text }],
      })));
      await startup.inject("first", runtime, assertActive);
      expect(runtime.appendDeveloperMessage).toHaveBeenCalledExactlyOnceWith(text);
    });
  });
});
