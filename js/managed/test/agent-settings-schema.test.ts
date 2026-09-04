import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import { initializeManagedAgentSettingsSchema } from "../src/agent-settings-schema";
import type { DurableAgentSession } from "../src/index";

describe("managed agent settings schema", () => {
  it("migrates retained settings before accepting Astra and remains idempotent", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    const stub = sessions.getByName(crypto.randomUUID());

    await runInDurableObject(stub, async (_session, state) => {
      state.storage.sql.exec(`
        DROP TABLE managed_agent_settings;
        CREATE TABLE managed_agent_settings (
          singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
          model TEXT NOT NULL CHECK (
            model IN ('gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna')
          ),
          thinking TEXT NOT NULL CHECK (thinking IN ('none', 'low', 'medium', 'high', 'xhigh', 'max')),
          reasoning_mode TEXT NOT NULL CHECK (reasoning_mode IN ('standard', 'pro')),
          fast_mode INTEGER NOT NULL CHECK (fast_mode IN (0, 1))
        );
        INSERT INTO managed_agent_settings
          (singleton, model, thinking, reasoning_mode, fast_mode)
        VALUES (1, 'gpt-5.6-terra', 'max', 'pro', 1);
      `);

      initializeManagedAgentSettingsSchema(state.storage);
      initializeManagedAgentSettingsSchema(state.storage);

      expect(state.storage.sql.exec<{
        model: string;
        thinking: string;
        reasoning_mode: string;
        fast_mode: number;
      }>(
        `SELECT model, thinking, reasoning_mode, fast_mode
         FROM managed_agent_settings WHERE singleton = 1`,
      ).one()).toEqual({
        model: "gpt-5.6-terra",
        thinking: "max",
        reasoning_mode: "pro",
        fast_mode: 1,
      });

      state.storage.sql.exec(
        "UPDATE managed_agent_settings SET model = 'gpt-6-astra' WHERE singleton = 1",
      );
      expect(state.storage.sql.exec<{ model: string }>(
        "SELECT model FROM managed_agent_settings WHERE singleton = 1",
      ).one().model).toBe("gpt-6-astra");
    });
  });
});
