import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { UserCredentialBroker } from "../src/broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import type { EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;

describe("manual credential vault control protocol", () => {
  it("stores secrets only in the encrypted envelope and returns stable metadata", async () => {
    const user = "manual-vault-roundtrip";
    const inputs = [
      {
        kind: "login",
        body: { name: "Example", username: "person@example.test", password: "login-secret" },
      },
      { kind: "api_key", body: { name: "Service", api_key: "api-key-secret" } },
      {
        kind: "card",
        body: {
          name: "Travel card",
          card_number: "4111111111111111",
          expiry_month: "09",
          expiry_year: "2031",
          cvv: "123",
          billing_zip: "10001",
        },
      },
      {
        kind: "address",
        body: {
          name: "Home",
          address_line_1: "1 Private Way",
          address_line_2: "Unit 2",
          city: "Athens",
          state: "Attica",
          zip: "10558",
          country: "Greece",
        },
      },
      {
        kind: "phone",
        body: { name: "Mobile", phone_number: "+30 690 000 0000" },
      },
    ] as const;

    const created: Array<Record<string, unknown> & {
      id: string;
      kind: string;
      name: string;
      created_at: number;
    }> = [];
    for (const input of inputs) {
      const response = await control(user, input.kind, input.body);
      expect(response.status).toBe(201);
      expect(response.headers.get("cache-control")).toBe("no-store");
      const metadata = await response.json<{
        id: string;
        kind: string;
        name: string;
        created_at: number;
      }>();
      expect(metadata).toEqual({
        id: expect.stringMatching(/^[A-Za-z0-9_-]{32}$/),
        kind: input.kind,
        name: input.body.name,
        created_at: expect.any(Number),
        ...(input.kind === "login" ? { username: input.body.username } : {}),
        ...(input.kind === "card" ? { last4: "1111" } : {}),
        ...(input.kind === "address" ? {
          address_line_1: input.body.address_line_1,
          address_line_2: input.body.address_line_2,
          city: input.body.city,
          state: input.body.state,
          zip: input.body.zip,
          country: input.body.country,
        } : {}),
        ...(input.kind === "phone" ? { phone_number: input.body.phone_number } : {}),
      });
      expect(JSON.stringify(metadata)).not.toMatch(
        /api-key-secret|login-secret|4111111111111111|"cvv"|"expiry_month"|"billing_zip"/,
      );
      created.push(metadata);
    }

    const status = await SELF.fetch(
      `https://broker.internal/users/${user}/credentials`,
    );
    expect(status.status).toBe(200);
    const statusBody = await status.json<{ vault: typeof created }>();
    const expected = [...created].sort(
      (left, right) => right.created_at - left.created_at
        || (left.id < right.id ? -1 : left.id > right.id ? 1 : 0),
    );
    expect(statusBody.vault).toEqual(expected);
    const publicJson = JSON.stringify(statusBody);
    expect(publicJson).not.toMatch(
      /api-key-secret|login-secret|4111111111111111|"cvv"|"expiry_month"|"billing_zip"/,
    );
    expect(publicJson).toContain("person@example.test");
    expect(publicJson).toContain("1 Private Way");
    expect(publicJson).toContain("+30 690 000 0000");

    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
      expect(row).toBeDefined();
      const raw = JSON.stringify(row);
      expect(raw).toContain("ciphertext");
      expect(raw).not.toMatch(
        /api-key-secret|login-secret|person@example|4111111111111111|Private Way|690 000/,
      );

      const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
      const opened = await vault.open<{ vault: Record<string, Record<string, unknown>> }>(
        row!.envelope,
      );
      expect(opened.value.vault[created[0]!.id]).toMatchObject({
        kind: "login",
        username: "person@example.test",
      });
      expect(opened.value.vault[created[0]!.id]).not.toHaveProperty("password");

      const entryRow = await state.storage.get<{ envelope: EncryptedEnvelope }>(
        `vault-entry:${created[0]!.id}`,
      );
      expect(entryRow).toBeDefined();
      expect(JSON.stringify(entryRow)).not.toMatch(/api-key-secret|login-secret|person@example/);
      const entryVault = new CredentialVault(
        workerEnv,
        `user/${state.id.toString()}/vault/${created[0]!.id}`,
      );
      const materialized = await entryVault.open<Record<string, unknown>>(entryRow!.envelope);
      expect(materialized.value).toMatchObject({
        id: created[0]!.id,
        kind: "login",
        username: "person@example.test",
        password: "login-secret",
      });
    });

    const materialized = await stub.fetch(
      `https://credentials.internal/v1/vault-entry/${created[0]!.id}`,
      { method: "POST" },
    );
    expect(materialized.status).toBe(200);
    expect(await materialized.json()).toMatchObject({
      id: created[0]!.id,
      username: "person@example.test",
      password: "login-secret",
    });

    const removed = await SELF.fetch(
      `https://broker.internal/users/${user}/credentials/vault/login/${created[0]!.id}`,
      { method: "DELETE" },
    );
    expect(removed.status).toBe(204);
    expect(await removed.text()).toBe("");
    const afterDelete = await SELF.fetch(
      `https://broker.internal/users/${user}/credentials`,
    );
    expect((await afterDelete.json<{ vault: typeof created }>()).vault)
      .toEqual(expected.filter(({ id }) => id !== created[0]!.id));
    const afterDeleteMaterialize = await stub.fetch(
      `https://credentials.internal/v1/vault-entry/${created[0]!.id}`,
      { method: "POST" },
    );
    expect(afterDeleteMaterialize.status).toBe(404);
  });

  it("rejects malformed, non-JSON, oversized, and broker-bypass payloads", async () => {
    const invalid = [
      { kind: "api_key", body: { name: "Service" } },
      { kind: "api_key", body: { name: "Service", api_key: "" } },
      { kind: "api_key", body: { name: "Service", api_key: "key", username: "user" } },
      { kind: "api_key", body: { name: "Service", api_key: "x".repeat(8193) } },
      { kind: "login", body: { name: "Site", username: "user" } },
      {
        kind: "login",
        body: { name: "Site", username: "user", password: "secret", extra: "field" },
      },
      {
        kind: "card",
        body: {
          name: "Card",
          card_number: "4111 1111 1111 XXXX",
          expiry_month: "13",
          expiry_year: "31",
          cvv: "12",
          billing_zip: "10001",
        },
      },
      {
        kind: "address",
        body: {
          name: "Home",
          address_line_1: "Street",
          address_line_2: "",
          city: "City",
          state: "State",
          zip: "Zip",
          country: "Country",
        },
      },
    ] as const;
    for (const [index, input] of invalid.entries()) {
      const response = await control(`manual-vault-invalid-${index}`, input.kind, input.body);
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_vault_entry" });
    }

    const nonJson = await SELF.fetch(
      "https://broker.internal/users/manual-vault-content/credentials/vault/phone",
      { method: "POST", body: JSON.stringify({ name: "Phone", phone_number: "+1" }) },
    );
    expect(nonJson.status).toBe(415);
    expect(await nonJson.json()).toEqual({ error: "invalid_content_type" });

    const oversized = await control("manual-vault-oversized", "login", {
      name: "Site",
      username: "user",
      password: "s".repeat(13 * 1024),
    });
    expect(oversized.status).toBe(413);
    expect(await oversized.json()).toEqual({ error: "body_too_large" });

    const direct = workerEnv.USER_CREDENTIALS.getByName("manual-vault-direct-invalid");
    const bypass = await direct.fetch("https://credentials.internal/v1/vault/phone", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "Phone", phone_number: "+1", leaked: "secret" }),
    });
    expect(bypass.status).toBe(400);
    expect(await bypass.json()).toEqual({ error: "invalid_vault_entry" });
  });
});

function control(user: string, kind: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.internal/users/${user}/credentials/vault/${kind}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
