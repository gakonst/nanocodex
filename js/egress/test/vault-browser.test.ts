import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";

const PASSWORD = "fixture-browser-password";
const ORIGIN = "https://www.amazon.com";
const workerEnv = env as unknown as EgressEnv;

describe("private browser Vault boundary", () => {
  it("requires owner and exact approved origin, preserving secrets through metadata-only approval", async () => {
    const owner = "vault-browser-owner";
    const subject = "B".repeat(43);
    const other = "C".repeat(43);
    for (const [id, user] of [[subject, owner], [other, "vault-browser-other"]]) {
      expect((await SELF.fetch(`https://broker.internal/subjects/${id}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ user_id: user }) })).status).toBe(200);
    }
    const created = await SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault/login`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "Amazon", username: "fixture@example.com", password: PASSWORD }),
    });
    expect(created.status).toBe(201);
    const entry = await created.json<{ id: string }>();
    const resolve = (who = subject, origin = ORIGIN) => handleEgress(new Request("https://browser-vault.internal/v1/login", {
      method: "POST", headers: { "content-type": "application/json", "x-nanocodex-subject": who },
      body: JSON.stringify({ vault_id: entry.id, expected_origin: origin }),
    }), workerEnv);
    expect((await resolve()).status).toBe(403);
    const approve = (origin: string, who = owner) => SELF.fetch(`https://broker.internal/users/${who}/credentials/vault/login/${entry.id}/origin`, {
      method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ browser_origin: origin }),
    });
    for (const invalid of ["http://www.amazon.com", "https://www.amazon.com/path", "https://person:secret@www.amazon.com", "https://www.amazon.com?x=y"]) {
      expect((await approve(invalid)).status).toBe(400);
    }
    expect((await approve(ORIGIN, "vault-browser-other")).status).toBe(404);
    const approved = await approve(ORIGIN);
    expect(approved.status).toBe(200);
    const metadata = await approved.json();
    expect(metadata).toMatchObject({ id: entry.id, browser_origin: ORIGIN });
    expect(JSON.stringify(metadata)).not.toContain(PASSWORD);
    const resolved = await resolve();
    expect(resolved.headers.get("cache-control")).toBe("no-store");
    expect(await resolved.json()).toEqual({ username: "fixture@example.com", password: PASSWORD });
    expect((await resolve(other)).status).toBe(403);
    for (const origin of ["https://amazon.com", "https://www.amazon.com.evil.example", "https://www.amazon.com:444"]) {
      expect((await resolve(subject, origin)).status).toBe(403);
    }
    await SELF.fetch(`https://broker.internal/users/${owner}/credentials/vault/login/${entry.id}`, { method: "DELETE" });
    expect((await resolve()).status).toBe(403);
  });
});
