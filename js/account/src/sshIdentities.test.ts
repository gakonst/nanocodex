import assert from "node:assert/strict";
import test from "node:test";
import { createSshIdentityPayload, createSshTargetPayload, decodeSshIdentities, sshIdentityPath } from "./sshIdentities.ts";

const target = { reference: "server", hostname: "server.example", username: "deploy", port: 22, hostKeySha256: "SHA256:" + "a".repeat(43) };
const privateKey = "-----BEGIN PRIVATE KEY-----\n" + "A".repeat(64) + "\n-----END PRIVATE KEY-----\n";

test("SSH onboarding requires a pinned target and projects only public key metadata", () => {
  assert.deepEqual(createSshTargetPayload(target), { hostname: target.hostname, username: target.username, port: 22, host_key_sha256: target.hostKeySha256 });
  assert.throws(() => createSshTargetPayload({ ...target, hostKeySha256: "unknown" }));
  const publicKey = "ecdsa-sha2-nistp256 AAAA";
  assert.deepEqual(decodeSshIdentities([{ reference: target.reference, ...createSshTargetPayload(target), public_key: publicKey, private_key: "must not project" }]), [
    { ...target, publicKey },
  ]);
  assert.throws(() => decodeSshIdentities([{ reference: target.reference, ...createSshTargetPayload(target), public_key: publicKey + "\ncommand=unexpected" }]));
});

test("SSH generation builds a target without requiring or projecting a private key", () => {
  const input = { ...target, private_key: "unused" };
  assert.deepEqual({ ...createSshTargetPayload(input), generate: true }, {
    hostname: target.hostname, username: target.username, port: 22, host_key_sha256: target.hostKeySha256, generate: true,
  });
  assert.equal(input.private_key, "unused");
  assert.equal(sshIdentityPath("server-1.example"), "/v1/credentials/ssh/server-1.example");
});

test("SSH targets reject unavailable or malformed hosts before uploading a key", () => {
  for (const hostname of [
    "", "Server.example", "server.example.", "user@server.example", "server.example:22", "server example", "-bad.example", "a..example",
    "localhost", "server.localhost", "server.local", "server.internal", "server.invalid", "server.test", "server.home.arpa",
    "999.1.2.3", "10.0.0.1", "127.0.0.1", "169.254.1.1", "172.16.0.1", "192.168.0.1", "100.64.0.1", "198.18.0.1", "224.0.0.1",
  ]) assert.throws(() => createSshTargetPayload({ ...target, hostname }), /public lowercase DNS name/, hostname);
  for (const hostname of ["server.example", "deep.host.example", "203.0.113.10", "172.32.0.1"]) {
    assert.equal(createSshTargetPayload({ ...target, hostname }).hostname, hostname);
  }
});

test("SSH references, ports, usernames, and trust pins are validated in both key modes", () => {
  for (const reference of ["", "constructor", "prototype", "__proto__", "../server", "a".repeat(65)]) {
    assert.throws(() => sshIdentityPath(reference));
    assert.throws(() => createSshTargetPayload({ ...target, reference }));
  }
  for (const invalid of [
    { port: 0 }, { port: 65_536 }, { port: 22.5 }, { port: NaN },
    { username: "" }, { username: "deploy root" }, { username: "deploy@host" },
    { hostKeySha256: "" }, { hostKeySha256: "SHA256:unknown" }, { hostKeySha256: target.hostKeySha256 + "\n" },
  ]) {
    assert.throws(() => createSshTargetPayload({ ...target, ...invalid }));
    assert.throws(() => createSshIdentityPayload({ ...target, ...invalid }, privateKey));
  }
});

test("SSH upload preserves PEM bytes and rejects oversized or unsupported files", () => {
  assert.deepEqual(createSshIdentityPayload(target, privateKey), { ...createSshTargetPayload(target), private_key: privateKey });
  for (const invalid of ["", "A".repeat(100), privateKey + "\0", privateKey + "a".repeat(64 * 1024),
    privateKey.replace("BEGIN PRIVATE KEY", "BEGIN ENCRYPTED PRIVATE KEY"),
    privateKey.replace("BEGIN PRIVATE KEY", "BEGIN OPENSSH PRIVATE KEY")]) {
    assert.throws(() => createSshIdentityPayload(target, invalid), /unencrypted PEM/);
  }
});

test("SSH metadata supports older identities without a public key and rejects malformed public keys", () => {
  const metadata = { reference: target.reference, ...createSshTargetPayload(target) };
  assert.deepEqual(decodeSshIdentities([metadata]), [target]);
  for (const public_key of [null, 1, "", "ssh-rsa AAAA\n", "ssh-rsa AAAA===", "ssh-rsa " + "A".repeat(16384),
    "command=unexpected ssh-rsa AAAA", "-----BEGIN PRIVATE KEY-----"]) {
    assert.throws(() => decodeSshIdentities([{ ...metadata, public_key }]), /Invalid SSH public key/);
  }
  for (const public_key of ["ssh-rsa AAAA", "ecdsa-sha2-nistp256 AAAA", "ecdsa-sha2-nistp384 AAAA", "ecdsa-sha2-nistp521 AAAA"]) {
    assert.equal(decodeSshIdentities([{ ...metadata, public_key }])[0]!.publicKey, public_key);
  }
});
