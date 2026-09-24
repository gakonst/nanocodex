import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import test from "node:test";
import { promisify } from "node:util";

// Each process starts with an empty module cache, as a fresh Worker does.
async function isolated(source) {
  const result = await promisify(execFile)(process.execPath, ["--input-type=module", "--eval", `
    import assert from "node:assert/strict";
    import { registerHooks } from "node:module";
    const leaf = ${JSON.stringify(new URL("../tools/ssh.mjs", import.meta.url).href)};
    const directArgs = ["-o", "PasswordRef=fixture", "-o", "StrictHostKeyChecking=no", "user@example.test", "--", "true"];
    const context = { cwd: "/", stdin: "", signal: new AbortController().signal };
    ${source}
  `], { timeout: 20_000 });
  assert.equal(result.stderr, "");
}

test("SSH discovery, validation, byte streams and brokered execution do not load SSH or Bash", async () => {
  await isolated(`
    registerHooks({ resolve(specifier, context, nextResolve) {
      if (specifier.startsWith("@microsoft/dev-tunnels-ssh") || specifier === "just-bash/browser") {
        throw new Error("unexpected eager dependency: " + specifier);
      }
      return nextResolve(specifier, context);
    } });
    const { createSshCommand, createWebStreamSshStream } = await import(leaf);
    let delegated = 0;
    const command = createSshCommand({ transport: "tcp",
      async openStream() { assert.fail("unexpected local connection"); },
      async resolvePassword() { assert.fail("unexpected credential lookup"); },
      async executeWithIdentityReference() {
        delegated++;
        return { stdout: "brokered", stderr: "", exitCode: 0 };
      },
    });
    assert.equal((await command.execute(["--help"], context)).exitCode, 0);
    assert.equal((await command.execute([], context)).exitCode, 2);
    const brokerArgs = ["-o", "IdentityRef=fixture", "user@example.test", "--", "true"];
    assert.equal((await command.execute(brokerArgs, context)).stdout, "brokered");
    const canceled = { ...context, signal: AbortSignal.abort(new Error("fixture canceled")) };
    for (const args of [brokerArgs, directArgs]) {
      assert.match((await command.execute(args, canceled)).stderr, /fixture canceled/);
    }
    assert.equal(delegated, 1);
    const stream = createWebStreamSshStream({
      readable: new ReadableStream({ start(controller) { controller.close(); } }),
      writable: new WritableStream(), async close() {},
    });
    assert.equal(await stream.read(1), null);
    stream.dispose();
  `);
});

test("cancellation while loading SSH prevents session creation and external connection", async () => {
  await isolated(`
    let release, started;
    globalThis.sshLoadGate = new Promise(resolve => { release = resolve; });
    const loading = new Promise(resolve => { started = resolve; });
    globalThis.sshLoadStarted = started;
    registerHooks({ resolve(specifier, context, nextResolve) {
      let source;
      if (specifier === "@microsoft/dev-tunnels-ssh") source = \`
        globalThis.sshLoadStarted(); await globalThis.sshLoadGate;
        export class CancellationTokenSource { constructor() { throw new Error("unexpected session creation"); } }
      \`;
      if (specifier === "@microsoft/dev-tunnels-ssh-keys") source = "await globalThis.sshLoadGate;";
      return source === undefined ? nextResolve(specifier, context)
        : { shortCircuit: true, url: "data:text/javascript," + encodeURIComponent(source) };
    } });
    const { createSshCommand } = await import(leaf);
    let opened = 0;
    const command = createSshCommand({ transport: "tcp",
      async openStream() { opened++; }, async resolvePassword() { return "fixture"; },
    });
    const controller = new AbortController();
    const pending = command.execute(directArgs, { ...context, signal: controller.signal });
    await loading;
    controller.abort(new Error("canceled during import"));
    release();
    const result = await pending;
    assert.equal(result.exitCode, 255);
    assert.match(result.stderr, /canceled during import/);
    assert.equal(opened, 0);
  `);
});

test("a failed SSH load does not poison later direct executions", async () => {
  await isolated(`
    let fail = true;
    registerHooks({ resolve(specifier, context, nextResolve) {
      if (specifier === "@microsoft/dev-tunnels-ssh" && context.parentURL === leaf && fail) {
        fail = false;
        throw new Error("transient SSH load failure");
      }
      return nextResolve(specifier, context);
    } });
    const { createSshCommand } = await import(leaf);
    let opened = 0;
    const command = createSshCommand({ transport: "tcp",
      async openStream() { opened++; throw new Error("transport reached"); },
      async resolvePassword() { return "fixture"; },
    });
    assert.match((await command.execute(directArgs, context)).stderr, /transient SSH load failure/);
    assert.equal(opened, 0);
    assert.match((await command.execute(directArgs, context)).stderr, /transport reached/);
    assert.equal(opened, 1);
  `);
});

test("key helpers retry a failed load and preserve the generated public key", async () => {
  await isolated(`
    let fail = true;
    registerHooks({ resolve(specifier, context, nextResolve) {
      if (specifier === "@microsoft/dev-tunnels-ssh-keys" && context.parentURL === leaf && fail) {
        fail = false;
        throw new Error("transient key load failure");
      }
      return nextResolve(specifier, context);
    } });
    const { createSshKeyPair, sshPublicKey } = await import(leaf);
    await assert.rejects(createSshKeyPair(), /transient key load failure/);
    const key = await createSshKeyPair();
    assert.equal(await sshPublicKey(key.privateKey), key.publicKey);
    assert.match(key.publicKey, /^ecdsa-sha2-nistp256 /);
  `);
});

test("cancellation during identity loading prevents external connection", async () => {
  await isolated(`
    const { createSshCommand, createSshKeyPair } = await import(leaf);
    const key = await createSshKeyPair();
    const controller = new AbortController();
    let opened = 0;
    const command = createSshCommand({ transport: "tcp",
      async openStream() { opened++; throw new Error("unexpected connection"); },
      async readIdentity() {
        controller.abort(new Error("canceled during identity read"));
        return key.privateKey;
      },
    });
    const result = await command.execute([
      "-i", "fixture", "-o", "StrictHostKeyChecking=no", "user@example.test", "--", "true",
    ], { ...context, signal: controller.signal });
    assert.equal(result.exitCode, 255);
    assert.match(result.stderr, /canceled during identity read/);
    assert.equal(opened, 0);
  `);
});
