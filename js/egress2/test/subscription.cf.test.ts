import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { UserCredentials } from "../src/index";

const credentialEnv = env as unknown as { USER_CREDENTIALS: DurableObjectNamespace<UserCredentials> };
const jwt = (exp: number) => `e30.${btoa(JSON.stringify({ exp, "https://api.openai.com/auth": {
  chatgpt_account_id: "synthetic-account", chatgpt_account_is_fedramp: false,
} })).replaceAll("=", "").replaceAll("+", "-").replaceAll("/", "_")}.signature`;

describe("actual workerd UserCredentials + compiled Rust subscription", () => {
  it("routes only the authenticated owner's credential and strips internal headers", async () => {
    const owner = `synthetic-api-${crypto.randomUUID()}`;
    const stub = credentialEnv.USER_CREDENTIALS.getByName(owner);
    await stub.putCredential("openai", "sk-synthetic-only");
    const request = (id: string, authorization = "Bearer NANOCODEX_PROVIDER_CREDENTIAL") => SELF.fetch(
      "https://api.openai.com/v1/responses", {
        method: "POST", headers: { "x-managed2-owner": id, "x-nanocodex-subject": "private-user",
          authorization, "content-type": "application/json" }, body: "{}",
      });
    const response = await request(owner);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ authorized: true, leakedOwner: false, leakedSubject: false });
    expect((await request(`unknown-${crypto.randomUUID()}`)).status).toBe(403);
    expect((await request(owner, "Bearer wrong")).status).toBe(400);
  });

  it("encrypts API keys in SQLite and rejects plaintext without fallback", async () => {
    const stub = credentialEnv.USER_CREDENTIALS.getByName(`synthetic-api-${crypto.randomUUID()}`);
    await stub.putCredential("openai", "sk-synthetic-only");
    const stored = await runInDurableObject(stub, async (_instance, state) => {
      const row = state.storage.sql.exec<{ value: string }>("SELECT value FROM credentials WHERE provider = ?", "openai").toArray()[0];
      return row?.value;
    });
    expect(stored).toMatch(/^v1:/);
    expect(stored).not.toContain("sk-synthetic-only");
    expect(await stub.getActiveCredential()).toEqual({ kind: "openai", secret: "sk-synthetic-only" });
    await runInDurableObject(stub, async (_instance, state) => {
      state.storage.sql.exec("UPDATE credentials SET value = ? WHERE provider = ?", "sk-synthetic-only", "openai");
    });
    // Catch within workerd; rejected RPC promises otherwise surface as pool unhandled rejections.
    const rejected = await runInDurableObject(stub, async instance => {
      try { await instance.getActiveCredential(); return false; }
      catch { return true; }
    });
    expect(rejected).toBe(true);
  });

  it("imports and rotates a rejected revision through the DO with no live network", async () => {
    const owner = `synthetic-owner-${crypto.randomUUID()}`;
    const stub = credentialEnv.USER_CREDENTIALS.getByName(owner);
    // Import a future token, then exercise the Rust lifecycle's
    // explicit rejected-revision recovery.
    const futureExpiry = 4_070_908_800_000;
    await stub.putChatGptCredential({ access_token: jwt(futureExpiry / 1000), refresh_token: "synthetic-refresh",
      account_id: "synthetic-account", expires_at: futureExpiry, fedramp: false });
    await runInDurableObject(stub, async (_instance, state) => {
      const row = await state.storage.get<{ revision: string; payload: string }>("subscription");
      expect(row?.payload).toMatch(/^v1:/);
      expect(row?.payload).not.toContain("synthetic-refresh");
      expect(row?.payload).not.toContain("synthetic-account");
      const sql = state.storage.sql.exec<{ value: string }>("SELECT value FROM credentials WHERE provider = ?", "active").toArray();
      expect(sql[0]?.value).toBe("chatgpt");
    });
    const initial = await stub.getActiveCredential();
    expect(initial?.kind).toBe("chatgpt");
    if (initial?.kind !== "chatgpt") throw Error("subscription unavailable");
    const recovered = await stub.recoverChatGptCredential(initial.revision!);
    expect(recovered?.kind).toBe("chatgpt");
    if (recovered?.kind !== "chatgpt") throw Error("recovery unavailable");
    expect(recovered.revision).not.toBe(initial.revision);
    expect(recovered.secret).not.toBe(initial.secret);
    await runInDurableObject(stub, async (_instance, state) => {
      const row = await state.storage.get<{ payload: string }>("subscription");
      expect(row?.payload).toMatch(/^v1:/);
      expect(row?.payload).not.toContain("synthetic-rotated");
    });
    const reread = await stub.getActiveCredential();
    expect(reread?.kind).toBe("chatgpt");
    if (reread?.kind === "chatgpt") expect(reread.secret).toBe(recovered.secret);
    const replacement = jwt(futureExpiry / 1000 - 60);
    await stub.putChatGptCredential({ access_token: replacement, refresh_token: "replacement-synthetic",
      account_id: "synthetic-account", expires_at: futureExpiry - 60_000, fedramp: false });
    const swapped = await stub.getActiveCredential();
    expect(swapped?.kind).toBe("chatgpt");
    if (swapped?.kind === "chatgpt") {
      expect(swapped.secret).toBe(replacement);
      expect(swapped.revision).not.toBe(recovered.revision);
    }
  });
});
