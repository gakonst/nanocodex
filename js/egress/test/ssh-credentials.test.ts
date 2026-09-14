import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { sshPublicKey } from "nanocodex/tools/ssh";
import type { UserCredentialBroker } from "../src/broker";
import type { EgressEnv } from "../src/egress";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";

it("generates a target-bound key in the encrypted vault and returns only its installable public key", async () => {
  const user = "ssh-key-generation", base = `https://broker.internal/users/${user}/credentials`;
  const body = { generate: true, hostname: "server.example", port: 22, username: "deploy", host_key_sha256: "SHA256:" + "a".repeat(43) };
  const put = () => SELF.fetch(base + "/ssh/server", { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  expect((await put()).status).toBe(204);
  const status = await (await SELF.fetch(base)).json<{ ssh: { public_key: string; reference: string }[] }>();
  expect(status.ssh[0]?.public_key).toMatch(/^ecdsa-sha2-nistp256 AAAA/);
  expect(JSON.stringify(status)).not.toMatch(/privateKey|private_key|PRIVATE KEY/);
  const workerEnv = env as unknown as EgressEnv;
  const stub = workerEnv.USER_CREDENTIALS.getByName(user);
  await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
    const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
    expect(JSON.stringify(row)).not.toContain("PRIVATE KEY");
    const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
    const opened = await vault.open<{ ssh: Record<string, { privateKey: string }> }>(row!.envelope);
    expect(await sshPublicKey(opened.value.ssh.server!.privateKey)).toBe(status.ssh[0]!.public_key);
  });
  expect((await put()).status).toBe(409);
  const again = await (await SELF.fetch(base)).json<typeof status>();
  expect(again.ssh[0]!.public_key).toBe(status.ssh[0]!.public_key);
  expect((await SELF.fetch(base + "/ssh/server", { method: "DELETE" })).status).toBe(204);
});
