import test from "node:test";
import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { connectComputerTools, createComputerTools, discoverComputer, ensureComputer } from "../index.mjs";

async function fixture(t, platform = "darwin") {
  const root = await mkdtemp(join(tmpdir(), "nanocodex-cua-setup-"));
  const originalPlatform = Object.getOwnPropertyDescriptor(process, "platform");
  const originalEnv = process.env;
  process.env = { HOME: root, PATH: "", NANOCODEX_DIR: join(root, "custom install root") };
  Object.defineProperty(process, "platform", { value: platform });
  t.after(async () => {
    process.env = originalEnv;
    Object.defineProperty(process, "platform", originalPlatform);
    await rm(root, { recursive: true, force: true });
  });
  const executable = join(process.env.NANOCODEX_DIR, "runtimes/openai-cua/hosts/fixture/cua-provider");
  const binary = join(root, "native helper ; literal");
  const calls = join(root, "setup-calls.jsonl");
  async function script(path, source) {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, `#!${process.execPath}\n${source}\n`, { mode: 0o755 });
  }
  async function receipt(command = executable, args = [], environment = {}) {
    const path = join(process.env.NANOCODEX_DIR || join(root, ".nanocodex"), "runtimes/openai-cua/provider.json");
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, JSON.stringify({ status: "installed", transport: "mcp", executable: command, args, environment }));
  }
  async function helper(mode = "ok") {
    await script(binary, `
      const fs = require('node:fs');
      fs.appendFileSync(${JSON.stringify(calls)}, JSON.stringify({ args: process.argv.slice(2), root: process.env.NANOCODEX_DIR, credentialPresent: 'NANOCODEX_API_KEY' in process.env }) + '\\n');
      if (${JSON.stringify(mode)} === 'failed') { console.error('fixture download unavailable'); process.exit(3); }
      if (${JSON.stringify(mode)} === 'malformed') { console.log('invalid receipt'); process.exit(0); }
      if (${JSON.stringify(mode)} === 'unsupported') { console.log(JSON.stringify({ status: 'unsupported' })); process.exit(0); }
      const executable = ${JSON.stringify(executable)};
      if (${JSON.stringify(mode)} !== 'missing') { fs.mkdirSync(require('node:path').dirname(executable), { recursive: true }); fs.writeFileSync(executable, '', { mode: 0o755 }); }
      const receipt = { status: 'installed', executable, transport: 'mcp', args: [], environment: {} };
      const path = require('node:path').join(process.env.NANOCODEX_DIR, 'runtimes/openai-cua/provider.json');
      fs.mkdirSync(require('node:path').dirname(path), { recursive: true });
      fs.writeFileSync(path, JSON.stringify(receipt));
      console.log(JSON.stringify(receipt));
    `);
  }
  return { root, executable, binary, calls, script, helper, receipt };
}

test("discovery never selects an adjacent retired companion and has no setup side effects", async t => {
  const f = await fixture(t);
  await f.helper();
  await f.script(join(f.root, "nanocodex-computer"), "process.exit(0)");
  assert.equal(await discoverComputer({ binary: f.binary }), undefined);
  await assert.rejects(readFile(f.calls), { code: "ENOENT" });
  await f.script(f.executable, "process.exit(0)");
  assert.equal(await discoverComputer({ binary: f.binary }), undefined, "an executable without a receipt is not selected");
  await f.receipt();
  assert.equal(await discoverComputer({ binary: f.binary }), f.executable);
  await assert.rejects(readFile(f.calls), { code: "ENOENT" });
  assert.equal(await ensureComputer({ binary: f.binary }), f.executable);
  assert.equal((await readFile(f.calls, "utf8")).trim().split("\n").length, 1);
});

test("explicit providers and disable sentinels bypass managed discovery and setup", async t => {
  const f = await fixture(t);
  await f.script(f.executable, "process.exit(0)");
  for (const value of ["off", "none", "0", "/trusted/custom provider"]) {
    process.env.NANOCODEX_COMPUTER = value;
    const expected = value.startsWith("/") ? value : undefined;
    assert.equal(await discoverComputer({ binary: f.binary }), expected);
    assert.equal(await ensureComputer({ binary: f.binary }), expected);
  }
  await assert.rejects(readFile(f.calls), { code: "ENOENT" });
});

test("setup uses literal native CLI arguments, preserves the configured root, and runs only once", async t => {
  const f = await fixture(t);
  await f.helper();
  process.env.NANOCODEX_API_KEY = "synthetic-credential-not-for-setup";
  assert.deepEqual(await Promise.all([ensureComputer({ binary: f.binary }), ensureComputer({ binary: f.binary })]), [f.executable, f.executable]);
  assert.equal(await ensureComputer({ binary: f.binary }), f.executable);
  assert.deepEqual((await readFile(f.calls, "utf8")).trim().split("\n").map(JSON.parse), [
    { args: ["computer", "setup"], root: process.env.NANOCODEX_DIR, credentialPresent: false },
  ]);
});

for (const mode of ["failed", "malformed", "unsupported", "missing"]) {
  test(`setup ${mode} surfaces an actionable error without selecting the legacy companion`, async t => {
    const f = await fixture(t);
    await f.helper(mode);
    await f.script(join(f.root, "nanocodex-computer"), "process.exit(0)");
    await assert.rejects(ensureComputer({ binary: f.binary }), /OpenAI CUA setup failed:.*[\s\S]*nanocodex2 computer setup/);
    if (mode === "failed") {
      await f.helper();
      assert.equal(await ensureComputer({ binary: f.binary }), f.executable, "A later explicit retry can recover");
    }
  });
}

test("a missing native helper explains how to recover", async t => {
  await fixture(t);
  await assert.rejects(ensureComputer(), /native helper.*Reinstall Nanocodex.*computer setup/);
});

test("unsupported platforms never discover a companion or attempt managed provisioning", async t => {
  const f = await fixture(t, "linux");
  const companion = join(f.root, "nanocodex-computer");
  await f.script(companion, "process.exit(0)");
  await f.script(f.executable, "process.exit(0)");
  assert.equal(await discoverComputer({ binary: f.binary }), undefined);
  assert.equal(await ensureComputer({ binary: f.binary }), undefined);
  await assert.rejects(readFile(f.calls), { code: "ENOENT" });
});

test("every provider uses exact MCP arguments regardless of retired environment flags", async t => {
  const f = await fixture(t);
  await f.script(f.executable, `
    if (process.argv.length !== 2) process.exit(9);
    require('node:readline').createInterface({ input: process.stdin }).on('line', line => {
      const request = JSON.parse(line);
      if (!request.id) return;
      const result = request.method === 'tools/list' ? { tools: [{ name: 'js', inputSchema: { type: 'object' } }] }
        : request.method === 'tools/call' ? { content: [{ type: 'text', text: 'managed MCP' }] } : {};
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');
    });
  `);
  const attachment = await connectComputerTools({ executable: f.executable });
  t.after(attachment.close);
  assert.equal((await attachment.tool("js").handler({}, { sessionId: "synthetic", signal: new AbortController().signal })).output[0].text, "managed MCP");
  assert.throws(() => createComputerTools({ executable: f.executable }), /require connectComputerTools/);
  process.env.NANOCODEX_COMPUTER_TRANSPORT = "legacy";
  const ignoredLegacy = await connectComputerTools({ executable: f.executable });
  t.after(ignoredLegacy.close);
  const explicit = await connectComputerTools({ executable: f.executable });
  t.after(explicit.close);
  delete process.env.NANOCODEX_COMPUTER_TRANSPORT;
  const custom = join(f.root, "custom-provider");
  await f.script(custom, await readFile(f.executable, "utf8").then(value => value.split("\n").slice(1).join("\n")));
  const direct = await connectComputerTools({ executable: custom });
  t.after(direct.close);
  process.env.NANOCODEX_COMPUTER_TRANSPORT = "mcp";
  const overridden = await connectComputerTools({ executable: custom });
  t.after(overridden.close);
});

test("the default install root follows HOME and unusable managed entries do not fall back", async t => {
  const f = await fixture(t);
  delete process.env.NANOCODEX_DIR;
  const managed = join(f.root, ".nanocodex/runtimes/openai-cua/hosts/fixture/cua-provider");
  const companion = join(f.root, "nanocodex-computer");
  await f.script(companion, "process.exit(0)");
  await mkdir(managed, { recursive: true });
  await f.receipt(managed);
  assert.equal(await discoverComputer({ binary: f.binary }), undefined);
  await rm(managed, { recursive: true });
  await f.script(managed, "process.exit(0)");
  await chmod(managed, 0o600);
  assert.equal(await discoverComputer({ binary: f.binary }), undefined);
  await chmod(managed, 0o755);
  assert.equal(await ensureComputer(), managed);
});

for (const platform of ["darwin", "win32"]) test(`${platform} consumes the managed receipt's exact command and environment through MCP`, async t => {
  const f = await fixture(t, platform);
  await f.script(f.executable, `
    if (process.argv[2] !== 'provider-entry' || process.env.CODEX_CLI_PATH !== 'signed-host-fixture') process.exit(8);
    require('node:readline').createInterface({input:process.stdin}).on('line', line => {
      const r=JSON.parse(line); if (!r.id) return;
      const result = r.method === 'tools/list' ? {tools:[{name:'js',inputSchema:{type:'object'}}]} : {};
      process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result})+'\\n');
    });
  `);
  const receipt = {status:'installed',transport:'mcp',executable:f.executable,args:['provider-entry'],environment:{CODEX_CLI_PATH:'signed-host-fixture'}};
  await writeFile(join(process.env.NANOCODEX_DIR,'runtimes/openai-cua/provider.json'), JSON.stringify(receipt));
  assert.equal(await discoverComputer({binary:f.binary}),f.executable);
  assert.equal(await ensureComputer(),f.executable);
  const computer=await connectComputerTools({executable:f.executable});
  t.after(computer.close);
  assert.deepEqual(computer.tools.map(tool=>tool.name),['mcp__cua_repl__js']);
});

test("installed helper selects its new host once even with an existing receipt", async t => {
  const f = await fixture(t);
  const previous = join(dirname(dirname(f.executable)), "previous", "cua-provider");
  await f.script(previous, "process.exit(0)");
  await f.receipt(previous);
  assert.equal(await discoverComputer(), previous);
  await f.helper();
  assert.equal(await ensureComputer({ binary: f.binary }), f.executable);
  assert.equal(await discoverComputer(), f.executable);
  assert.equal(await ensureComputer({ binary: f.binary }), f.executable);
  assert.equal((await readFile(f.calls, "utf8")).trim().split("\n").length, 1);
  assert.equal(await readFile(previous, "utf8").then(source => source.includes("process.exit(0)")), true);
});

test("Mac discovery does not fall back to the old generated launcher", async t => {
  const f = await fixture(t);
  await f.script(join(process.env.NANOCODEX_DIR, "runtimes/openai-cua/current/cua-provider"), "process.exit(0)");
  assert.equal(await discoverComputer(), undefined);
  await assert.rejects(ensureComputer(), /native helper/);
});

for (const invalid of [null, { executable: "relative" }, { environment: [] }]) {
  test(`managed discovery rejects malformed receipt ${JSON.stringify(invalid)}`, async t => {
    const f = await fixture(t);
    await f.receipt();
    const path = join(process.env.NANOCODEX_DIR, "runtimes/openai-cua/provider.json");
    const receipt = invalid === null ? null : { ...JSON.parse(await readFile(path, "utf8")), ...invalid };
    await writeFile(path, JSON.stringify(receipt));
    await assert.rejects(discoverComputer(), /Invalid managed CUA receipt/);
  });
}

for (const platform of ["darwin", "win32"]) test(`${platform} explicit provider connects despite a malformed managed receipt`, async t => {
  const f = await fixture(t, platform);
  const custom = join(f.root, "explicit-provider");
  await f.script(custom, `
    if (process.argv[2] !== 'explicit-entry' || process.env.EXPLICIT_PROVIDER !== 'fixture') process.exit(8);
    require('node:readline').createInterface({ input: process.stdin }).on('line', line => {
      const request = JSON.parse(line);
      if (!request.id) return;
      const result = request.method === 'tools/list' ? { tools: [{ name: 'js', inputSchema: { type: 'object' } }] }
        : request.method === 'tools/call' ? { content: [{ type: 'text', text: 'explicit MCP' }] } : {};
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');
    });
  `);
  await f.receipt();
  process.env.NANOCODEX_COMPUTER = custom;
  for (const malformed of ['invalid JSON', 'null']) {
    await writeFile(join(process.env.NANOCODEX_DIR, "runtimes/openai-cua/provider.json"), malformed);
    assert.equal(await discoverComputer(), custom);
    assert.equal(await ensureComputer({ binary: f.binary }), custom);
    const options = { executable: custom, args: ['explicit-entry'], environment: { EXPLICIT_PROVIDER: 'fixture' } };
    const connected = await connectComputerTools(options);
    t.after(connected.close);
    const direct = createComputerTools({ ...options, definitions: connected.definitions });
    t.after(direct.close);
    for (const attachment of [connected, direct]) {
      const result = await attachment.tool("js").handler({}, { sessionId: "synthetic-explicit", signal: new AbortController().signal });
      assert.equal(result.output[0].text, "explicit MCP");
    }
  }
  await assert.rejects(readFile(f.calls), { code: "ENOENT" });
});
