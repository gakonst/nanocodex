import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { UserCredentialBroker } from "../src/broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import type { EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const JAR_ID = "browser-cookie-jar-test-01";
const ORIGIN = "https://app.example.com";
const PROFILE_ID = "browser-instance-01";
const STORE_ID = "0";

describe("encrypted browser cookie jar control protocol", () => {
  it("keeps cookie material in a distinct ciphertext row and lists metadata only", async () => {
    const user = "browser-cookie-roundtrip";
    const cookies = fixtureCookies("roundtrip-secret");
    const created = await upsert(user, JAR_ID, 0, cookies);
    expect(created.status).toBe(201);
    expectNoStore(created);
    expect(await created.json()).toEqual({
      id: JAR_ID,
      origin: ORIGIN,
      profile_id: PROFILE_ID,
      store_id: STORE_ID,
      revision: 1,
      cookie_count: 2,
      updated_at: expect.any(Number),
    });

    const listed = await SELF.fetch(controlUrl(user));
    expect(listed.status).toBe(200);
    expectNoStore(listed);
    const listing = await listed.json<Record<string, unknown>>();
    expect(listing).toEqual({
      browser_cookie_jars: [{
        id: JAR_ID,
        origin: ORIGIN,
        profile_id: PROFILE_ID,
        store_id: STORE_ID,
        revision: 1,
        cookie_count: 2,
        updated_at: expect.any(Number),
      }],
    });
    expect(JSON.stringify(listing)).not.toMatch(/session_cookie|persistent_cookie|roundtrip-secret/);

    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const aggregate = await state.storage.get<{ envelope: EncryptedEnvelope }>(
        "credential-state",
      );
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>(
        `browser-cookie-jar:${JAR_ID}`,
      );
      expect(aggregate).toBeDefined();
      expect(row).toBeDefined();
      expect(JSON.stringify({ aggregate, row })).not.toMatch(
        /session_cookie|persistent_cookie|roundtrip-secret/,
      );

      const aggregateVault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
      const aggregateValue = await aggregateVault.open<{
        browserCookieJars: Record<string, Record<string, unknown>>;
      }>(aggregate!.envelope);
      expect(aggregateValue.value.browserCookieJars[JAR_ID]).toMatchObject({
        id: JAR_ID,
        origin: ORIGIN,
        profileId: PROFILE_ID,
        storeId: STORE_ID,
        revision: 1,
        cookieCount: 2,
      });
      expect(JSON.stringify(aggregateValue.value)).not.toMatch(
        /session_cookie|persistent_cookie|roundtrip-secret|"cookies"/,
      );

      const jarVault = new CredentialVault(
        workerEnv,
        `user/${state.id.toString()}/browser-cookie-jar/${JAR_ID}`,
      );
      const jar = await jarVault.open<Record<string, unknown>>(row!.envelope);
      expect(jar.value).toMatchObject({
        schemaVersion: 1,
        id: JAR_ID,
        origin: ORIGIN,
        profileId: PROFILE_ID,
        storeId: STORE_ID,
        revision: 1,
        cookies,
      });
    });

    const materialized = await materialize(user, JAR_ID);
    expect(materialized.status).toBe(200);
    expectNoStore(materialized);
    expect(await materialized.json()).toMatchObject({
      schema_version: 1,
      id: JAR_ID,
      origin: ORIGIN,
      profile_id: PROFILE_ID,
      store_id: STORE_ID,
      revision: 1,
      cookies,
    });
  });

  it("binds ciphertext AAD and jar state to the derived user", async () => {
    const userA = "browser-cookie-account-a";
    const userB = "browser-cookie-account-b";
    expect((await upsert(userA, JAR_ID, 0, fixtureCookies("account-a-secret"))).status)
      .toBe(201);
    expect((await upsert(userB, JAR_ID, 0, fixtureCookies("account-b-secret"))).status)
      .toBe(201);

    let envelopeA: EncryptedEnvelope | undefined;
    const stubA = workerEnv.USER_CREDENTIALS.getByName(userA);
    await runInDurableObject(stubA, async (_instance: UserCredentialBroker, state) => {
      envelopeA = (await state.storage.get<{ envelope: EncryptedEnvelope }>(
        `browser-cookie-jar:${JAR_ID}`,
      ))?.envelope;
    });
    expect(envelopeA).toBeDefined();

    const stubB = workerEnv.USER_CREDENTIALS.getByName(userB);
    await runInDurableObject(stubB, async (_instance: UserCredentialBroker, state) => {
      const wrongAccountVault = new CredentialVault(
        workerEnv,
        `user/${state.id.toString()}/browser-cookie-jar/${JAR_ID}`,
      );
      await expect(wrongAccountVault.open(envelopeA!)).rejects.toThrow();
    });

    const a = await (await materialize(userA, JAR_ID)).text();
    const b = await (await materialize(userB, JAR_ID)).text();
    expect(a).toContain("account-a-secret");
    expect(a).not.toContain("account-b-secret");
    expect(b).toContain("account-b-secret");
    expect(b).not.toContain("account-a-secret");
  });

  it("decrypts a names-only projection with sorted unique names and no cookie material", async () => {
    const user = "browser-cookie-names";
    const secret = "names-projection-secret-sentinel";
    const cookies = fixtureCookies(secret);
    expect((await upsert(user, JAR_ID, 0, [
      ...cookies,
      { ...cookies[0], value: `${secret}-second-identity`, path: "/other" },
    ])).status).toBe(201);

    const projected = await names(user, JAR_ID);
    expect(projected.status).toBe(200);
    expectNoStore(projected);
    const text = await projected.text();
    expect(JSON.parse(text)).toEqual({
      id: JAR_ID,
      origin: ORIGIN,
      profile_id: PROFILE_ID,
      store_id: STORE_ID,
      revision: 1,
      updated_at: expect.any(Number),
      cookie_count: 3,
      cookie_names: ["persistent_cookie", "session_cookie"],
    });
    expect(text).not.toContain(secret);
    expect(text).not.toMatch(/"(?:cookies|value|domain|path)"/);

    const wrongBinding = await rawNames(user, JAR_ID, {
      origin: ORIGIN,
      profile_id: "other-profile",
      store_id: STORE_ID,
    });
    expect(wrongBinding.status).toBe(409);
    expect(await wrongBinding.json()).toEqual({ error: "browser_cookie_jar_binding_conflict" });
  });

  it("enforces canonical origin, cookie domain, partition, store, and CAS fences", async () => {
    const user = "browser-cookie-validation";
    const invalidCases: unknown[] = [
      body(0, fixtureCookies("secret"), { origin: "HTTPS://app.example.com" }),
      body(0, fixtureCookies("secret"), { origin: "http://app.example.com" }),
      body(0, [{ ...fixtureCookies("secret")[0], domain: ".other.example" }]),
      body(0, [{ ...fixtureCookies("secret")[0], storeId: "other" }]),
      body(0, [{
        ...fixtureCookies("secret")[0],
        partitionKey: { topLevelSite: "https://TOP.example" },
      }]),
      body(0, [{
        ...fixtureCookies("secret")[0],
        partitionKey: { topLevelSite: "https://top.example", opaque: true },
      }]),
    ];
    for (const invalid of invalidCases) {
      const response = await rawUpsert(user, JAR_ID, invalid);
      expect(response.status).toBe(400);
      expect(await response.json()).toEqual({ error: "invalid_browser_cookie_jar" });
    }

    const created = await upsert(user, JAR_ID, 0, fixtureCookies("current-secret"));
    expect(created.status).toBe(201);
    const stale = await upsert(user, JAR_ID, 0, fixtureCookies("stale-secret"));
    expect(stale.status).toBe(409);
    expect(await stale.json()).toEqual({ error: "browser_cookie_jar_revision_conflict" });
    expect(await (await materialize(user, JAR_ID)).text()).not.toContain("stale-secret");

    const wrongBinding = await SELF.fetch(`${controlUrl(user)}/${JAR_ID}/materialize`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ origin: ORIGIN, profile_id: "other-profile", store_id: STORE_ID }),
    });
    expect(wrongBinding.status).toBe(409);
    expect(await wrongBinding.json()).toEqual({ error: "browser_cookie_jar_binding_conflict" });
  });

  it("atomically deletes metadata and sealed material at the current revision", async () => {
    const user = "browser-cookie-delete";
    expect((await upsert(user, JAR_ID, 0, fixtureCookies("delete-secret"))).status).toBe(201);

    const stale = await remove(user, JAR_ID, 0);
    expect(stale.status).toBe(409);
    expect(await stale.json()).toEqual({ error: "browser_cookie_jar_revision_conflict" });

    const removed = await remove(user, JAR_ID, 1);
    expect(removed.status).toBe(204);
    expectNoStore(removed);
    expect(await removed.text()).toBe("");
    expect(await (await SELF.fetch(controlUrl(user))).json()).toEqual({ browser_cookie_jars: [] });
    expect((await materialize(user, JAR_ID)).status).toBe(404);

    const stub = workerEnv.USER_CREDENTIALS.getByName(user);
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      expect(await state.storage.get(`browser-cookie-jar:${JAR_ID}`)).toBeUndefined();
    });
  });
});

function fixtureCookies(secret: string): readonly Record<string, unknown>[] {
  return [
    {
      name: "session_cookie",
      value: secret,
      domain: "app.example.com",
      path: "/",
      hostOnly: true,
      secure: true,
      httpOnly: true,
      sameSite: "lax",
      session: true,
      storeId: STORE_ID,
    },
    {
      name: "persistent_cookie",
      value: `${secret}-persistent`,
      domain: ".example.com",
      path: "/account",
      hostOnly: false,
      secure: true,
      httpOnly: false,
      sameSite: "no_restriction",
      session: false,
      expirationDate: 4_102_444_800,
      storeId: STORE_ID,
      partitionKey: {
        topLevelSite: "https://top.example",
        hasCrossSiteAncestor: false,
      },
    },
  ];
}

function body(
  revision: number,
  cookies: unknown,
  override: Partial<Record<"origin" | "profile_id" | "store_id", string>> = {},
): Record<string, unknown> {
  return {
    schema_version: 1,
    origin: ORIGIN,
    profile_id: PROFILE_ID,
    store_id: STORE_ID,
    revision,
    cookies,
    ...override,
  };
}

function controlUrl(user: string): string {
  return `https://broker.internal/users/${user}/credentials/browser-cookie-jars`;
}

function rawUpsert(user: string, id: string, value: unknown): Promise<Response> {
  return SELF.fetch(`${controlUrl(user)}/${id}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(value),
  });
}

function upsert(
  user: string,
  id: string,
  revision: number,
  cookies: unknown,
): Promise<Response> {
  return rawUpsert(user, id, body(revision, cookies));
}

function materialize(user: string, id: string): Promise<Response> {
  return SELF.fetch(`${controlUrl(user)}/${id}/materialize`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ origin: ORIGIN, profile_id: PROFILE_ID, store_id: STORE_ID }),
  });
}

function rawNames(user: string, id: string, binding: Record<string, string>): Promise<Response> {
  return SELF.fetch(`${controlUrl(user)}/${id}/names`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(binding),
  });
}

function names(user: string, id: string): Promise<Response> {
  return rawNames(user, id, {
    origin: ORIGIN,
    profile_id: PROFILE_ID,
    store_id: STORE_ID,
  });
}

function remove(user: string, id: string, revision: number): Promise<Response> {
  return SELF.fetch(`${controlUrl(user)}/${id}`, {
    method: "DELETE",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      origin: ORIGIN,
      profile_id: PROFILE_ID,
      store_id: STORE_ID,
      revision,
    }),
  });
}

function expectNoStore(response: Response): void {
  expect(response.headers.get("cache-control")).toBe("no-store");
  expect(response.headers.get("pragma")).toBe("no-cache");
}
