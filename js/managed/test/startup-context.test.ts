import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { PromptInput } from "nanocodex";
import type { DurableAgentSession } from "../src/index";
import { ManagedStartupContext, startupQuery } from "../src/startup-context";

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

describe("managed first-prompt preload boundary", () => {
  it("bounds Unicode queries and excludes attachment URLs", () => {
    const long = startupQuery(`  ${"😀".repeat(200)} tail`);
    expect(new TextEncoder().encode(long).length).toBe(512);
    expect(long).not.toContain("�");
    expect(startupQuery([{ type: "text", text: " copper\n finch " }, {
      type: "image", image_url: "https://private.example/secret",
    }])).toBe("copper finch [image]");
    expect(startupQuery([])).toBe("conversation context");
  });

  it("runs both lookups concurrently once, retains results, and preserves the original multimodal prompt", async () => {
    await withStartup(async (startup, state) => {
      const input: PromptInput = [{ type: "text", text: firstPrompt }, { type: "image", image_url: "data:image/png;base64,test" }];
      startup.reserve("first", input);
      state.storage.sql.exec("UPDATE session_state SET accepted_turns = 1");
      startup.reserve("second", "different question");
      const entered: string[] = [];
      let release!: () => void;
      const bothEntered = new Promise<void>((resolve) => { release = resolve; });
      const execute = vi.fn(async (name: string) => {
        entered.push(name);
        if (entered.length === 2) release();
        await bothEntered;
        return name === "memory" ? { operation: "scan", abstained: true, candidates: [] } : { sessions: [] };
      });
      const enriched = await startup.prepare("first", input, execute, () => {});
      expect(entered).toEqual(["find_session", "memory"]);
      expect(execute.mock.calls).toHaveLength(2);
      expect(enriched.slice(0, 2)).toEqual(input);
      expect(JSON.stringify(enriched)).toContain("preloaded_tool_results");
      const restored = new ManagedStartupContext(state.storage);
      await expect(restored.prepare("first", input, execute, () => {})).resolves.toEqual(enriched);
      await expect(restored.prepare("second", "different question", execute, () => {})).resolves.toBe("different question");
      expect(execute).toHaveBeenCalledTimes(2);
      expect(restored.events("first").map((event) => [event.type, event.payload.tool])).toEqual([
        ["tool.call", "find_session"], ["tool.result", "find_session"],
        ["tool.call", "memory"], ["tool.result", "memory"],
      ]);
      expect(() => state.storage.transactionSync(() => {
        restored.markPublished("first");
        throw new Error("rollback dispatch");
      })).toThrow("rollback dispatch");
      expect(restored.events("first")).toHaveLength(4);
      restored.markPublished("first");
      expect(new ManagedStartupContext(state.storage).events("first")).toEqual([]);
    });
  });

  it.each(["existing", "multiplayer"])("does not add preloads to %s conversations", async (kind) => {
    await withStartup(async (startup, state) => {
      state.storage.sql.exec(kind === "existing"
        ? "UPDATE session_state SET accepted_turns = 3"
        : "UPDATE session_state SET runtime_profile = 'multiplayer'");
      startup.reserve("later", firstPrompt);
      const execute = vi.fn();
      await expect(startup.prepare("later", firstPrompt, execute, () => {})).resolves.toBe(firstPrompt);
      expect(execute).not.toHaveBeenCalled();
    });
  });

  it("keeps an authorized result when the other lookup fails without leaking internal errors", async () => {
    await withStartup(async (startup) => {
      startup.reserve("first", firstPrompt);
      const input = await startup.prepare("first", firstPrompt, async (name) => {
        if (name === "memory") throw Object.assign(new Error("provider token SECRET https://internal"), { code: "forbidden" });
        return { sessions: [{ session_id: "candidate" }] };
      }, () => {});
      expect(JSON.stringify(input)).toContain("candidate");
      expect(JSON.stringify(input)).toContain("forbidden");
      expect(JSON.stringify(input)).not.toMatch(/SECRET|https:\/\/internal/);
      expect(startup.events("first").filter((event) => event.type === "tool.result")
        .map((event) => event.payload.status)).toEqual(["completed", "failed"]);
    });
  });

  it("does not persist late lookup results after the owning agent is fenced", async () => {
    await withStartup(async (startup) => {
      startup.reserve("first", firstPrompt);
      let active = true;
      await expect(startup.prepare("first", firstPrompt, async () => {
        active = false;
        return { sessions: [] };
      }, () => { if (!active) throw new Error("fenced"); })).rejects.toThrow("fenced");
      expect(startup.events("first")).toEqual([]);
    });
  });
});
