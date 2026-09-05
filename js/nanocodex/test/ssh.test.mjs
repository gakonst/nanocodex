import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import test from "node:test";

import {
  BaseStream,
  ChannelRequestType,
  CommandRequestMessage,
  SshAlgorithms,
  SshAuthenticationType,
  SshServerSession,
  SshSessionConfiguration,
} from "@microsoft/dev-tunnels-ssh";
import { exportPrivateKey } from "@microsoft/dev-tunnels-ssh-keys";

import { createSshCommand, createWebStreamSshStream } from "../tools/ssh.mjs";

test("SSH rejects unsafe or incomplete invocations before opening a transport", async () => {
  let opened = 0;
  const command = createSshCommand({
    transport: "tcp",
    async openStream() { opened += 1; throw new Error("unexpected open"); },
    async readIdentity() { throw new Error("unexpected identity read"); },
  });
  const context = { cwd: "/workspace", stdin: "", signal: new AbortController().signal };

  assert.match((await command.execute(["--help"], context)).stdout, /\[USER@\]HOST/);
  assert.match(
    (await command.execute(["-i", "id", "user@example.com", "--", "true"], context)).stderr,
    /HostKeySHA256/,
  );
  assert.match(
    (await command.execute([
      "-i", "id", "-o", "StrictHostKeyChecking=no", "https://example.com", "--", "true",
    ], context)).stderr,
    /target must be \[USER@\]HOST/,
  );
  assert.equal(opened, 0);
});

test("IdentityRef delegates an opaque reference and exact target without opening locally", async () => {
  let delegated;
  const command = createSshCommand({
    transport: "tcp",
    async openStream() { throw new Error("unexpected local transport"); },
    async executeWithIdentityReference(request) {
      delegated = request;
      return { stdout: "brokered\n", stderr: "", exitCode: 0 };
    },
  });
  const context = { cwd: "/workspace", stdin: "", signal: new AbortController().signal };
  const result = await command.execute([
    "-p", "2222", "-o", "IdentityRef=production", "deploy@example.com",
    "--", "printf", "%s", "hello world",
  ], context);

  assert.deepEqual(result, { stdout: "brokered\n", stderr: "", exitCode: 0 });
  assert.deepEqual(delegated, {
    identityReference: "production",
    endpoint: { hostname: "example.com", port: 2222 },
    username: "deploy",
    commandArgs: ["printf", "%s", "hello world"],
  });
  assert.match(
    (await command.execute([
      "-o", "IdentityRef=production", "-o", "StrictHostKeyChecking=no",
      "deploy@example.com", "--", "true",
    ], context)).stderr,
    /broker-owned host-key verification/,
  );
});

test("Cloudflare byte streams preserve SSH read boundaries and writes", async () => {
  const input = new TransformStream();
  const output = new TransformStream();
  const inputWriter = input.writable.getWriter();
  const outputReader = output.readable.getReader();
  let closed = false;
  const stream = createWebStreamSshStream({
    readable: input.readable,
    writable: output.writable,
    async close() { closed = true; },
  });

  const incoming = inputWriter.write(Uint8Array.from([1, 2, 3, 4]));
  assert.deepEqual([...await stream.read(2)], [1, 2]);
  await incoming;
  assert.deepEqual([...await stream.read(4)], [3, 4]);

  const written = outputReader.read();
  await stream.write(Uint8Array.from([5, 6]));
  assert.deepEqual([...(await written).value], [5, 6]);
  stream.dispose();
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(closed, true);
});

test("Just Bash SSH authenticates and executes a remote command over byte streams", async () => {
  const [clientStream, serverStream] = memorySshStreamPair();
  const server = new SshServerSession(new SshSessionConfiguration());
  const hostKey = await SshAlgorithms.publicKey.rsaWithSha256.generateKeyPair();
  server.credentials.publicKeys.push(hostKey);
  server.onAuthenticating((event) => {
    if (event.authenticationType === SshAuthenticationType.clientPassword
      && event.username === "worker"
      && event.password === "test-password") {
      event.authenticationPromise = Promise.resolve({ username: "worker" });
    } else event.authenticationPromise = Promise.resolve(null);
  });
  let remoteCommand;
  server.onChannelOpening((event) => {
    event.channel.onRequest((request) => {
      if (request.requestType !== ChannelRequestType.command) return;
      const command = request.request.convertTo(new CommandRequestMessage());
      remoteCommand = command.command;
      request.isAuthorized = true;
      queueMicrotask(async () => {
        await event.channel.send(Buffer.from("worker-ssh\n"));
        await event.channel.close(0);
      });
    });
  });
  const connected = server.connect(serverStream);
  const command = createSshCommand({
    transport: "tcp",
    async openStream() { return clientStream; },
    async resolvePassword(reference) {
      assert.equal(reference, "test-secret");
      return "test-password";
    },
  });

  const result = await command.execute([
    "-l", "worker",
    "-o", "PasswordRef=test-secret",
    "-o", "StrictHostKeyChecking=no",
    "example.test", "--", "printf", "worker-ssh",
  ], { cwd: "/workspace", stdin: "", signal: new AbortController().signal });
  await connected;

  assert.deepEqual(result, { stdout: "worker-ssh\n", stderr: "", exitCode: 0 });
  assert.equal(remoteCommand, "printf worker-ssh");
  server.dispose();
  hostKey.dispose();
});

test("Just Bash SSH imports a private key and proves possession to the server", async () => {
  const [clientStream, serverStream] = memorySshStreamPair();
  const server = new SshServerSession(new SshSessionConfiguration());
  const hostKey = await SshAlgorithms.publicKey.rsaWithSha256.generateKeyPair();
  const clientKey = await SshAlgorithms.publicKey.rsaWithSha256.generateKeyPair();
  const privateKey = await exportPrivateKey(clientKey);
  server.credentials.publicKeys.push(hostKey);
  server.onAuthenticating((event) => {
    const publicKey = event.authenticationType === SshAuthenticationType.clientPublicKeyQuery
      || event.authenticationType === SshAuthenticationType.clientPublicKey;
    event.authenticationPromise = Promise.resolve(
      publicKey && event.username === "deploy" ? { username: "deploy" } : null,
    );
  });
  server.onChannelOpening((event) => {
    event.channel.onRequest((request) => {
      if (request.requestType !== ChannelRequestType.command) return;
      request.isAuthorized = true;
      queueMicrotask(async () => {
        await event.channel.send(Buffer.from("key-authenticated\n"));
        await event.channel.close(0);
      });
    });
  });
  const connected = server.connect(serverStream);
  const command = createSshCommand({
    transport: "tcp",
    async openStream() { return clientStream; },
    async readIdentity(path) {
      assert.equal(path, "brokered-identity");
      return privateKey;
    },
  });

  const result = await command.execute([
    "-i", "brokered-identity",
    "-o", "StrictHostKeyChecking=no",
    "deploy@example.test", "--", "true",
  ], { cwd: "/workspace", stdin: "", signal: new AbortController().signal });
  await connected;

  assert.deepEqual(result, { stdout: "key-authenticated\n", stderr: "", exitCode: 0 });
  server.dispose();
  clientKey.dispose();
  hostKey.dispose();
});

function memorySshStreamPair() {
  class MemorySshStream extends BaseStream {
    other;
    async write(data) { this.other.onData(Buffer.from(data)); }
    async close(error) {
      if (error) this.other.onError(error);
      else this.other.onEnd();
      this.dispose();
    }
  }
  const first = new MemorySshStream();
  const second = new MemorySshStream();
  first.other = second;
  second.other = first;
  return [first, second];
}
