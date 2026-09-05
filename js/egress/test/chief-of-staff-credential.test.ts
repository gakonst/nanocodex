import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { UserCredentialBroker } from "../src/broker";
import type { EncryptedEnvelope } from "../src/credential-vault";
import type { EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const USER_ID = "11111111-1111-4111-8111-111111111111";

describe("Chief of Staff model authority", () => {
  it("installs one service credential idempotently without exposing it publicly", async () => {
    const stub = workerEnv.USER_CREDENTIALS.getByName(USER_ID);
    const install = () => stub.fetch(
      "https://credentials.internal/v1/chief-of-staff/openai-key",
      { method: "PUT" },
    );

    expect((await install()).status).toBe(204);
    expect((await install()).status).toBe(204);
    const credential = await stub.fetch("https://credentials.internal/v1/credential", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ recover: false }),
    });
    expect(await credential.json()).toEqual({
      kind: "openai",
      revision: 0,
      secret: "sk-chief-of-staff-test-secret",
    });

    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
      expect(JSON.stringify(row)).not.toContain("sk-chief-of-staff-test-secret");
    });
    const publicAttempt = await SELF.fetch(
      `https://broker.internal/users/${USER_ID}/credentials/chief-of-staff/openai-key`,
      { method: "PUT" },
    );
    expect(publicAttempt.status).not.toBe(204);
  });
});
