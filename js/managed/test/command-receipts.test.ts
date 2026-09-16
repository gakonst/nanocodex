import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { CommandReceipts } from "../src/command-receipts";
import type { DurableAgentSession } from "../src/index";

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;

describe("durable control command receipts", () => {
  it("replays acceptance across owner reconstruction and rejects changed payloads and authority", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      let receipts = new CommandReceipts(state.storage);
      const execute = vi.fn(async () => Response.json({ turn_id: "turn", state: "steering" }, { status: 202 }));
      expect((await receipts.run("turn", "request", "account", "steer", { input: "hello" }, execute)).status).toBe(202);
      receipts = new CommandReceipts(state.storage);
      expect((await receipts.run("turn", "request", "account", "steer", { input: "hello" }, execute)).status).toBe(202);
      expect(execute).toHaveBeenCalledOnce();
      expect(await receipts.status("turn", "request", "account").json()).toMatchObject({ status: "accepted" });
      expect((await receipts.run("turn", "request", "account", "steer", { input: "changed" }, execute)).status).toBe(409);
      expect((await receipts.run("other-turn", "request", "account", "cancel", null, execute)).status).toBe(409);
      expect((await receipts.run("turn", "request", "other-grant", "steer", { input: "hello" }, execute)).status).toBe(403);
      expect(receipts.status("turn", "request", "other-grant").status).toBe(404);
    });
  });

  it("retains uncertainty across a crash after dispatch rather than resubmitting", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      const receipts = new CommandReceipts(state.storage);
      let effects = 0;
      await expect(receipts.run("turn", "request", "account", "steer", "input", async () => {
        effects++;
        throw new Error("connection lost after admission");
      })).rejects.toThrow("connection lost");
      const restarted = new CommandReceipts(state.storage);
      const retry = await restarted.run("turn", "request", "account", "steer", "input", async () => {
        effects++; return Response.json({});
      });
      expect(await retry.json()).toMatchObject({ error: "command_delivery_unknown" });
      expect(await restarted.status("turn", "request", "account").json()).toMatchObject({ status: "unknown" });
      expect(effects).toBe(1);
    });
  });

  it("allows an identified steer to retry a proven pre-dispatch recovery response", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      const receipts = new CommandReceipts(state.storage);
      const first = await receipts.run("turn", "request", "account", "steer", "input",
        async () => Response.json({ error: "turn_recovering" }, { status: 503 }));
      expect(first.status).toBe(503);
      const execute = vi.fn(async () => Response.json({ state: "steering" }, { status: 202 }));
      expect((await receipts.run("turn", "request", "account", "steer", "input", execute)).status).toBe(202);
      expect((await receipts.run("turn", "request", "account", "steer", "input", execute)).status).toBe(202);
      expect(execute).toHaveBeenCalledOnce();
    });
  });

  it("persists the intent before dispatch and refuses to execute after a storage failure", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      const failing = { sql: state.storage.sql, sync: async () => { throw new Error("disk unavailable"); } } as unknown as DurableObjectStorage;
      const receipts = new CommandReceipts(failing);
      const execute = vi.fn(async () => Response.json({}));
      await expect(receipts.run("turn", "request", "account", "cancel", null, execute)).rejects.toThrow("disk unavailable");
      expect(execute).not.toHaveBeenCalled();
    });
  });
});
