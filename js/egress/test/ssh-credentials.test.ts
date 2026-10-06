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

it("imports once, reconciles exact retries, and refuses different keys or targets under concurrent PUTs", async () => {
  const user = "ssh-key-import", base = `https://broker.internal/users/${user}/credentials`;
  const { createSshKeyPair } = await import("nanocodex/tools/ssh");
  const [first, second] = await Promise.all([createSshKeyPair(), createSshKeyPair()]);
  const body = { private_key: first.privateKey, hostname: "server.example", port: 22, username: "deploy", host_key_sha256: "SHA256:" + "a".repeat(43) };
  const put = (reference: string, payload: typeof body) => SELF.fetch(`${base}/ssh/${reference}`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(payload),
  });
  expect((await put("imported", body)).status).toBe(204);
  const workerEnv = env as unknown as EgressEnv;
  const stub = workerEnv.USER_CREDENTIALS.getByName(user);
  const retainedRow = () => runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => state.storage.get("credential-state"));
  const before = await retainedRow();
  expect((await put("imported", body)).status).toBe(204);
  expect(await retainedRow()).toEqual(before); // Exact reconciliation does not reseal or rewrite.
  for (const changed of [
    { ...body, private_key: second.privateKey }, { ...body, hostname: "other.example" },
    { ...body, port: 2222 }, { ...body, username: "other" },
    { ...body, host_key_sha256: "SHA256:" + "b".repeat(43) },
  ]) {
    const response = await put("imported", changed);
    expect(response.status).toBe(409);
    expect(await response.text()).not.toContain("PRIVATE KEY");
    expect(await retainedRow()).toEqual(before);
  }
  const raced = await Promise.all([put("raced", body), put("raced", { ...body, private_key: second.privateKey })]);
  expect(raced.map(r => r.status).sort()).toEqual([204, 409]);
  const winner = raced[0]!.status === 204 ? first : second;
  const status = await (await SELF.fetch(base)).json<{ ssh: { public_key: string; reference: string }[] }>();
  expect(status.ssh.find(i => i.reference === "imported")?.public_key).toBe(first.publicKey);
  expect(status.ssh.find(i => i.reference === "raced")?.public_key).toBe(winner.publicKey);
  expect(JSON.stringify(status)).not.toMatch(/privateKey|private_key|PRIVATE KEY/);
  await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
    const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
    expect(JSON.stringify(row)).not.toContain("PRIVATE KEY");
    const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
    const opened = await vault.open<{ ssh: Record<string, { privateKey: string }> }>(row!.envelope);
    expect(opened.value.ssh.imported!.privateKey).toBe(first.privateKey);
    expect(opened.value.ssh.raced!.privateKey).toBe(winner.privateKey);
  });
  console.info("SSH import public broker journey: exact retry unchanged; five key/target conflicts 409; concurrent different imports one204/one409; encrypted state and metadata preserve winner.");
});
