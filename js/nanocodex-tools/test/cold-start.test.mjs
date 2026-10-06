import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import test from "node:test";

// Output alone cannot expose eager module evaluation. Fresh processes observe
// the real module loader while exercising the public command factories/API.
async function isolated(source) {
  const { stdout, stderr } = await promisify(execFile)(process.execPath, ["--input-type=module", "--eval", `
    import assert from "node:assert/strict";
    import { registerHooks } from "node:module";
    const expensive = /^(isomorphic-git(?:\\/|$)|modern-tar$|unpdf$|just-bash\\/browser$|diff$)/;
    registerHooks({ resolve(specifier, context, nextResolve) {
      if (expensive.test(specifier)) throw new Error("unexpected eager dependency: " + specifier);
      return nextResolve(specifier, context);
    } });
    ${source}
    console.log("cold commands completed without Git, archive, PDF, Bash, or diff evaluation");
  `], { timeout: 20_000 });
  assert.equal(stderr, "");
  assert.match(stdout, /cold commands completed/);
}

test("tools discovery, gh API and PDF help keep execution-only dependencies cold", async () => {
  await isolated(`
    const tools = await import(${JSON.stringify(new URL("../dist/index.js", import.meta.url).href)});
    const gh = tools.createGhCommand(async (url) => {
      assert.equal(url, "https://api.github.com/user");
      return { status: 200, body: new TextEncoder().encode('{"login":"fixture"}') };
    });
    assert.equal(typeof gh.execute, "function");
    assert.equal((await gh.execute(["auth", "status"])).stdout,
      "Logged in to github.com as fixture through the connected account.\\n");
    const filesystem = () => { throw new Error("help must not open the filesystem"); };
    const git = tools.createGitCommand(async () => assert.fail("unexpected request"), filesystem);
    assert.equal(typeof git.execute, "function");
    const pdf = tools.createPdfTextCommand(filesystem);
    assert.equal(typeof pdf.execute, "function");
    assert.equal((await pdf.execute(["--help"])).exitCode, 0);
  `);
});

test("browser thread metadata and gh API keep Git and diff cold", async () => {
  await isolated(`
    const { browserThread } = await import(${JSON.stringify(new URL("../../nanocodex/tools/browser/threadGit.mjs", import.meta.url).href)});
    const { createGhCompatibilityCommand, validateBrowserArtifactSource } = await import(
      ${JSON.stringify(new URL("../../nanocodex/tools/browser/browserShell.mjs", import.meta.url).href)});
    const thread = browserThread("11111111-1111-4111-8111-111111111111", "https://fixture.example");
    assert.equal(thread.branch, "nanocodex");
    validateBrowserArtifactSource("function App() { return null; }");
    const gh = createGhCompatibilityCommand({}, thread, (name, execute) => ({ name, execute }), {
      async fetch(url) {
        assert.equal(url, "https://api.github.com/user");
        return { status: 200, body: new TextEncoder().encode('{"login":"fixture"}') };
      },
    });
    assert.equal(typeof gh.execute, "function");
    assert.equal((await gh.execute(["auth", "status"])).stdout,
      "Logged in to github.com as fixture through the connected account.\\n");
  `);
});
