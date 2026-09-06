import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const origin = "https://nanocodex.internal";

async function seed(plan: string, marker: string) {
  const user = `history-${marker}`;
  const subject = marker.padEnd(64, "x");
  const expires = Math.ceil((Date.now() + 3_600_000) / 1000);
  const secret = `e30.${btoa(JSON.stringify({ exp: expires, "https://api.openai.com/auth": {
    chatgpt_account_id: "account-history", chatgpt_plan_type: plan,
  } })).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "")}.signature`;
  const imported = await SELF.fetch(`https://broker.internal/users/${user}/credentials/chatgpt`, {
    method: "PUT", headers: { "content-type": "application/json" },
    body: JSON.stringify({ access_token: secret, refresh_token: "test-refresh", account_id: "account-history", expires_at: expires * 1000, fedramp: false }),
  });
  expect(imported.status).toBe(204);
  const bound = await SELF.fetch(`https://broker.internal/subjects/${subject}`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ user_id: user }),
  });
  expect(bound.status).toBe(200);
  return { subject, secret };
}

function headers(subject: string) {
  return { authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "x-nanocodex-subject": subject, "content-type": "application/json", "session-id": "context-session", "x-openai-tool-output-truncation-policy": '{"mode":"tokens","limit":200000}' };
}

function operation(subject: string, path = "alpha/notes/v2/write_file", changes: Record<string, string> = {}) {
  return new Request(`${origin}/v1/${path}`, {
    method: "POST", headers: { ...headers(subject), ...changes },
    body: JSON.stringify({ path: "progress", content: "checkpoint", context: { session_id: "context-session", current_agent_name: "/root" } }),
  });
}

describe("private history/notes credential boundary", () => {
  it("uses the authoritative subscription gate and returns no credentials", async () => {
    for (const plan of ["plus", "pro", "prolite", "business", "free", "enterprise"]) {
      const { subject, secret } = await seed(plan, `gate-${plan}`);
      const result = await SELF.fetch(`${origin}/.well-known/nanocodex/context-management`, { headers: headers(subject) });
      const text = await result.text();
      expect(JSON.parse(text)).toEqual({ enabled: ["plus", "pro", "prolite"].includes(plan) });
      expect(text).not.toContain(secret);
    }
  });

  it("binds notes context, injects private authentication, and preserves encrypted output", async () => {
    const { subject, secret } = await seed("plus", "private-write");
    let observed: Request | undefined;
    const directEnv = { ...workerEnv };
    delete directEnv.CHATGPT_EGRESS;
    const result = await handleEgress(operation(subject), directEnv, undefined, (async (input) => {
      observed = input as Request;
      return Response.json({ encrypted_output: "opaque-result" }, { headers: { "set-cookie": "provider-private" } });
    }) as typeof fetch);
    expect(result.status).toBe(200);
    expect(await result.json()).toEqual({ encrypted_output: "opaque-result" });
    expect(result.headers.get("set-cookie")).toBeNull();
    expect(observed?.url).toBe("https://chatgpt.com/backend-api/codex/alpha/notes/v2/write_file");
    expect(observed?.headers.get("authorization")).toBe(`Bearer ${secret}`);
    expect(observed?.headers.get("chatgpt-account-id")).toBe("account-history");
    expect(observed?.headers.get("x-nanocodex-subject")).toBeNull();
    expect(observed?.headers.get("x-openai-encrypted-tool-arguments")).toBe("true");
    expect(JSON.parse(observed!.headers.get("x-openai-tool-output-truncation-policy")!)).toEqual({ mode: "tokens", limit: 200000 });
    expect((await observed!.json<{ context: unknown }>()).context).toEqual({ session_id: "context-session", current_agent_name: "/root" });
  });

  it("denies unsupported plans, routes, caller credentials, and mismatched context before egress", async () => {
    const plus = await seed("plus", "denial-plus");
    const business = await seed("business", "denial-business");
    const requests = [
      operation(business.subject), operation(plus.subject, "alpha/notes/v2/delete_all"),
      operation(plus.subject, undefined, { authorization: "Bearer caller-secret" }),
      operation(plus.subject, undefined, { "chatgpt-account-id": "spoof" }),
      operation(plus.subject, undefined, { "session-id": "other-session" }),
      operation(plus.subject, undefined, { "x-openai-tool-output-truncation-policy": '{"mode":"tokens","limit":-1}' }),
    ];
    for (const request of requests) {
      let called = false;
      const response = await handleEgress(request, workerEnv, undefined, (async () => { called = true; return Response.json({}); }) as typeof fetch);
      expect(response.status).toBeGreaterThanOrEqual(400);
      expect(called).toBe(false);
      expect(await response.text()).not.toMatch(/caller-secret|test-refresh|signature/);
    }
  });
});
