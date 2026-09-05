import { env } from "cloudflare:workers";
import { SELF } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";

import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const PASSWORD = "jit-login-secret";

describe("manual vault private egress", () => {
  it("materializes and injects only at the final fetch and returns status only", async () => {
    const user = "vault-egress-jit";
    const ownerSubject = subject("vault-egress-jit");
    const id = await createLogin(user);
    await bindSubject(ownerSubject, user);
    let captured: Readonly<{
      authorization: string | null;
      body: string;
      redirect: string;
      username: string | null;
      url: string;
    }> | undefined;
    const info = vi.spyOn(console, "info").mockImplementation(() => undefined);
    try {
      const response = await handleEgress(vaultRequest(ownerSubject, {
        vault_id: id,
        url: "https://merchant.example.com/checkout?login={{NANOCODEX_VAULT_USERNAME}}",
        method: "POST",
        headers: {
          authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
          "content-type": "application/json",
          "x-login": "{{NANOCODEX_VAULT_USERNAME}}",
        },
        body: '{"password":"{{NANOCODEX_VAULT_PASSWORD}}"}',
      }), workerEnv, undefined, (async (input: RequestInfo | URL, init?: RequestInit) => {
        const request = input instanceof Request ? input : new Request(input, init);
        captured = {
          authorization: request.headers.get("authorization"),
          body: await request.clone().text(),
          redirect: request.redirect,
          username: request.headers.get("x-login"),
          url: request.url,
        };
        return new Response(`echo:${PASSWORD}`, {
          status: 201,
          headers: {
            authorization: `Bearer ${PASSWORD}`,
            location: `https://attacker.example/${PASSWORD}`,
            "x-echo": PASSWORD,
          },
        });
      }) as typeof fetch);

      expect(captured).toEqual({
        authorization: `Basic ${btoa(`person@example.com:${PASSWORD}`)}`,
        body: `{"password":"${PASSWORD}"}`,
        redirect: "manual",
        username: "person@example.com",
        url: "https://merchant.example.com/checkout?login={{NANOCODEX_VAULT_USERNAME}}",
      });
      expect(response.status).toBe(200);
      expect(response.headers.get("cache-control")).toBe("no-store");
      expect(response.headers.get("authorization")).toBeNull();
      expect(response.headers.get("location")).toBeNull();
      expect(response.headers.get("x-echo")).toBeNull();
      expect(await response.json()).toEqual({ status: 201, ok: true });
      expect(JSON.stringify(info.mock.calls)).not.toContain(PASSWORD);
      expect(JSON.stringify(info.mock.calls)).not.toContain(id);
      expect(JSON.stringify(info.mock.calls)).not.toContain(user);
    } finally {
      info.mockRestore();
    }
  });

  it("uses API keys through bearer and custom headers without exposing secrets", async () => {
    const user = "vault-api-key";
    const owner = subject("vault-api-key");
    const secret = "fixture-api-key";
    const created = await SELF.fetch(`https://broker.internal/users/${user}/credentials/vault/api_key`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "Service", api_key: secret }),
    });
    expect(created.status).toBe(201);
    const metadata = await created.json<{ id: string }>();
    expect(JSON.stringify(metadata)).not.toContain(secret);
    await bindSubject(owner, user);
    const outbound = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : new Request(input, init);
      expect(request.headers.get("authorization")).toBe(`Bearer ${secret}`);
      expect(request.headers.get("x-api-key")).toBe(secret);
      return new Response(secret, { headers: { "x-secret": secret } });
    });
    const envelope = {
      vault_id: metadata.id, url: "https://service.example.com/api", method: "POST",
      headers: { authorization: "Bearer {{NANOCODEX_VAULT_API_KEY}}", "x-api-key": "{{NANOCODEX_VAULT_API_KEY}}" },
    };
    const response = await handleEgress(vaultRequest(owner, envelope), workerEnv, undefined, outbound as typeof fetch);
    expect(await response.json()).toEqual({ status: 200, ok: true });
    expect(response.headers.get("x-secret")).toBeNull();
    expect(outbound).toHaveBeenCalledTimes(1);
    const mismatch = await handleEgress(vaultRequest(owner, {
      ...envelope, headers: { authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}" },
    }), workerEnv, undefined, outbound as typeof fetch);
    expect(mismatch.status).toBe(403);
    await bindSubject(subject("other-api-key"), "other-api-key");
    const crossAccount = await handleEgress(vaultRequest(subject("other-api-key"), envelope), workerEnv, undefined, outbound as typeof fetch);
    expect(crossAccount.status).not.toBe(200);
    const deleted = await SELF.fetch(`https://broker.internal/users/${user}/credentials/vault/api_key/${metadata.id}`, { method: "DELETE" });
    expect(deleted.status).toBe(204);
    const revoked = await handleEgress(vaultRequest(owner, envelope), workerEnv, undefined, outbound as typeof fetch);
    expect(revoked.status).toBe(409);
    expect(outbound).toHaveBeenCalledTimes(1);
  });

  it("injects every closed card placeholder and no partial token", async () => {
    const user = "vault-egress-card";
    const ownerSubject = subject("vault-egress-card");
    const id = await createCard(user);
    await bindSubject(ownerSubject, user);
    let captured = "";
    const response = await handleEgress(vaultRequest(ownerSubject, {
      vault_id: id,
      url: "https://payments.example.com/charge",
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        number: "{{NANOCODEX_VAULT_CARD_NUMBER}}",
        month: "{{NANOCODEX_VAULT_EXPIRY_MONTH}}",
        year: "{{NANOCODEX_VAULT_EXPIRY_YEAR}}",
        cvv: "{{NANOCODEX_VAULT_CVV}}",
        zip: "{{NANOCODEX_VAULT_BILLING_ZIP}}",
        untouched: "NANOCODEX_CARD_NUMBER",
      }),
    }), workerEnv, undefined, (async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : new Request(input, init);
      captured = await request.text();
      return new Response(null, { status: 204 });
    }) as typeof fetch);

    expect(JSON.parse(captured)).toEqual({
      number: "4111 1111 1111 1111",
      month: "09",
      year: "2031",
      cvv: "123",
      zip: "10001",
      untouched: "NANOCODEX_CARD_NUMBER",
    });
    expect(await response.json()).toEqual({ status: 204, ok: true });
  });

  it("denies cross-account and nonexistent vault items", async () => {
    const owner = "vault-egress-owner";
    const other = "vault-egress-other";
    const otherSubject = subject("vault-egress-other");
    const id = await createLogin(owner);
    await bindSubject(otherSubject, other);
    const upstream = vi.fn(async () => new Response(null, { status: 204 }));

    for (const vaultId of [id, "Z".repeat(32)]) {
      const response = await handleEgress(vaultRequest(otherSubject, loginEnvelope(vaultId)),
        workerEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(409);
      expect(await response.json()).toEqual({ error: "vault_entry_unavailable" });
    }
    expect(upstream).not.toHaveBeenCalled();
  });

  it("denies placeholders that do not match the selected vault kind", async () => {
    const user = "vault-egress-kind";
    const ownerSubject = subject("vault-egress-kind");
    const id = await createLogin(user);
    await bindSubject(ownerSubject, user);
    const upstream = vi.fn(async () => new Response(null, { status: 204 }));
    const response = await handleEgress(vaultRequest(ownerSubject, {
      vault_id: id,
      url: "https://merchant.example.com/pay",
      method: "POST",
      headers: {},
      body: "{{NANOCODEX_VAULT_CARD_NUMBER}}",
    }), workerEnv, undefined, upstream as typeof fetch);

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: "vault_entry_kind_mismatch" });
    expect(upstream).not.toHaveBeenCalled();
  });

  it("denies raw credential headers even when another field has a placeholder", async () => {
    const upstream = vi.fn(async () => new Response(null, { status: 204 }));
    const response = await handleEgress(vaultRequest(subject("vault-egress-raw"), {
      vault_id: "R".repeat(32),
      url: "https://merchant.example.com/login",
      method: "POST",
      headers: { authorization: "Bearer raw-caller-token" },
      body: "{{NANOCODEX_VAULT_PASSWORD}}",
    }), workerEnv, undefined, upstream as typeof fetch);

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: "vault_raw_credential_denied" });
    expect(upstream).not.toHaveBeenCalled();

    const cookie = await handleEgress(vaultRequest(subject("vault-egress-cookie"), {
      vault_id: "C".repeat(32),
      url: "https://merchant.example.com/login",
      method: "POST",
      headers: { cookie: "{{NANOCODEX_VAULT_PASSWORD}}" },
      body: "{{NANOCODEX_VAULT_PASSWORD}}",
    }), workerEnv, undefined, upstream as typeof fetch);
    expect(cookie.status).toBe(403);
    expect(await cookie.json()).toEqual({ error: "vault_header_denied" });
    expect(upstream).not.toHaveBeenCalled();
  });

  it("denies private and provider destinations before vault materialization", async () => {
    const upstream = vi.fn(async () => new Response(null, { status: 204 }));
    for (const target of [
      "http://127.0.0.1/steal",
      "https://service.internal/steal",
      "https://api.github.com/user",
    ]) {
      const response = await handleEgress(vaultRequest(subject("vault-egress-target"), {
        ...loginEnvelope("T".repeat(32)),
        url: target,
      }), workerEnv, undefined, upstream as typeof fetch);
      expect(response.status).toBe(403);
      expect(await response.json()).toEqual({ error: "vault_destination_denied" });
    }
    expect(upstream).not.toHaveBeenCalled();
  });

  it("does not follow or expose an upstream redirect", async () => {
    const user = "vault-egress-redirect";
    const ownerSubject = subject("vault-egress-redirect");
    const id = await createLogin(user);
    await bindSubject(ownerSubject, user);
    const upstream = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : new Request(input, init);
      expect(request.redirect).toBe("manual");
      return new Response(PASSWORD, {
        status: 302,
        headers: { location: "http://127.0.0.1/private", "x-secret": PASSWORD },
      });
    });

    const response = await handleEgress(vaultRequest(ownerSubject, loginEnvelope(id)),
      workerEnv, undefined, upstream as typeof fetch);
    expect(upstream).toHaveBeenCalledTimes(1);
    expect(response.status).toBe(200);
    expect(response.headers.get("location")).toBeNull();
    expect(response.headers.get("x-secret")).toBeNull();
    expect(await response.json()).toEqual({ status: 302, ok: false });
  });
});

function loginEnvelope(vaultId: string): Record<string, unknown> {
  return {
    vault_id: vaultId,
    url: "https://merchant.example.com/login",
    method: "POST",
    headers: { authorization: "Basic {{NANOCODEX_VAULT_BASIC}}" },
  };
}

function vaultRequest(ownerSubject: string, envelope: unknown): Request {
  return new Request("https://vault-egress.internal/v1/request", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-nanocodex-subject": ownerSubject,
    },
    body: JSON.stringify(envelope),
  });
}

async function createLogin(user: string): Promise<string> {
  const response = await SELF.fetch(
    `https://broker.internal/users/${user}/credentials/vault/login`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        name: "Example login",
        username: "person@example.com",
        password: PASSWORD,
      }),
    },
  );
  expect(response.status).toBe(201);
  return (await response.json<{ id: string }>()).id;
}

async function createCard(user: string): Promise<string> {
  const response = await SELF.fetch(
    `https://broker.internal/users/${user}/credentials/vault/card`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        name: "Example card",
        card_number: "4111 1111 1111 1111",
        expiry_month: "09",
        expiry_year: "2031",
        cvv: "123",
        billing_zip: "10001",
      }),
    },
  );
  expect(response.status).toBe(201);
  return (await response.json<{ id: string }>()).id;
}

async function bindSubject(value: string, user: string): Promise<void> {
  const response = await SELF.fetch(`https://broker.internal/subjects/${value}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ user_id: user }),
  });
  expect(response.status).toBe(200);
}

function subject(seed: string): string {
  const encoded = btoa(seed).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  return `${encoded}${"S".repeat(43)}`.slice(0, 43);
}
