import assert from "node:assert/strict";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { packTar } from "modern-tar";
import { createGhCommand, createGitCommand } from "../dist/shell.js";

for (const flag of [["--jq", ".name"], ["--jq=.name"], ["-q", ".name"], ["-q.name"]]) {
  test(`gh api rejects ${flag.join(" ")} before requesting unfiltered data`, async () => {
    let called = false;
    const gh = createGhCommand(async () => { called = true; throw new Error("unexpected request"); });
    const result = await gh.execute(["api", "repos/fixture/repo", ...flag]);
    assert.equal(result.exitCode, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /gh api ENDPOINT \| jq 'FILTER'/);
    assert.equal(called, false);
  });
}

for (const args of [
  ["api", "user", "--paginate"], ["api", "user", "--method"],
  ["api", "user", "extra"], ["repo", "view", "fixture/repo", "--json", "name"],
  ["repo", "list", "--json", "name"], ["auth", "status", "--show-token"],
  ["pr", "list", "--repo", "fixture/repo", "--state", "closed"],
]) {
  test(`gh rejects unsupported or malformed arguments: ${args.join(" ")}`, async () => {
    let called = false;
    const result = await createGhCommand(async () => { called = true; }).execute(args);
    assert.equal(result.exitCode, 1);
    assert.equal(result.stdout, "");
    assert.equal(called, false);
  });
}

test("gh api retains supported method and field handling", async () => {
  const calls = [];
  const result = await createGhCommand(async (url, options) => {
    calls.push({ url, options });
    return { status: 200, body: new TextEncoder().encode('{"name":"fixture"}') };
  }).execute(["api", "search/repositories", "--method", "GET", "-f", "q=fixture"]);
  assert.equal(result.exitCode, 0);
  assert.deepEqual(JSON.parse(result.stdout), { name: "fixture" });
  assert.equal(calls[0].url, "https://api.github.com/search/repositories?q=fixture");
  assert.equal(calls[0].options.method, "GET");
});

for (const linkname of ["target", "later", "link", "../escape", "/outside"]) {
  test(`clone preserves no partial destination for unsupported link target ${linkname}`, async () => {
    const data = gzipSync(await packTar([
      { header: { name: "repo/", type: "directory", size: 0 } },
      { header: { name: "repo/target", type: "file", size: 6 }, body: "source" },
      { header: { name: "repo/link", type: "symlink", size: 0, linkname } },
      { header: { name: "repo/later", type: "file", size: 5 }, body: "later" },
    ]));
    const paths = new Map();
    let writeSettled = false;
    let cleaned = false;
    const workspace = {
      root: "/brain",
      list: async (path) => [...paths.values()].filter((entry) => entry.path.slice(0, entry.path.lastIndexOf("/")) === path),
      mkdir: async (path) => { paths.set(path, { path, kind: "directory" }); },
      writeFile: async (path) => {
        await new Promise((resolve) => setTimeout(resolve, 5));
        paths.set(path, { path, kind: "file" });
        writeSettled = true;
      },
      remove: async (path, options) => {
        assert.equal(writeSettled, true);
        assert.equal(options.recursive, true);
        for (const key of paths.keys()) if (key === path || key.startsWith(`${path}/`)) paths.delete(key);
        cleaned = true;
      },
    };
    const git = createGitCommand(async () => ({ status: 200, body: data }), () => workspace);
    const result = await git.execute(["clone", "https://github.com/fixture/repo.git", "repo"]);
    assert.equal(result.exitCode, 1);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /requested destination requires symlink support in the workspace backend/);
    assert.equal(cleaned, true);
    assert.equal(paths.size, 0);
  });
}
