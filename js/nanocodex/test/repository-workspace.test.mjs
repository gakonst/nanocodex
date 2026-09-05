import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import * as NodeWorkspace from "../node/workspace.mjs";
import { materializeRepositoryWorkspace } from "../tools/repository-workspace.mjs";

test("materializes once, reopens without network, and preserves retained work", async () => {
  const fixture = await gitFixture();
  const destination = await mkdtemp(join(tmpdir(), "nanocodex-materialized-"));
  try {
    const workspace = await NodeWorkspace.open({ path: destination });
    const seedUrl = `${fixture.url}/git/${fixture.head}`;
    const first = await materializeRepositoryWorkspace({
      workspace,
      fetch,
      seedUrl,
      head: fixture.head,
      branch: "main",
      directory: "source",
      writableRemote: { url: "https://example.com/write.git", branch: "changes" },
    });
    assert.deepEqual(first, {
      branch: "main",
      directory: "/workspace/source",
      gitDirectory: "/workspace/source/.git",
      head: fixture.head,
      markerPath: "/workspace/source/.git/nanocodex-repository.json",
      seedUrl,
      writableRemote: {
        name: "origin",
        url: "https://example.com/write.git",
        branch: "changes",
      },
    });
    assert.equal(await readFile(join(destination, "source/README.md"), "utf8"), "immutable\n");

    await workspace.writeFile("source/notes.txt", "retained dirty work\n");
    const reopened = await materializeRepositoryWorkspace({
      workspace,
      fetch: async () => { throw new Error("reopen must not use the network"); },
      seedUrl,
      head: fixture.head,
      branch: "main",
      directory: "source",
      writableRemote: { url: "https://example.com/write.git", branch: "changes" },
    });
    assert.deepEqual(reopened, first);
    assert.equal(await readFile(join(destination, "source/notes.txt"), "utf8"), "retained dirty work\n");
  } finally {
    await fixture.close();
    await rm(destination, { recursive: true, force: true });
  }
});

test("retries by cleaning only the validated unmarked child and rejects unsafe inputs", async () => {
  const fixture = await gitFixture();
  const destination = await mkdtemp(join(tmpdir(), "nanocodex-materialized-"));
  try {
    const workspace = await NodeWorkspace.open({ path: destination });
    await mkdir(join(destination, "source/.git"), { recursive: true });
    await writeFile(join(destination, "source/partial"), "failed clone");
    await writeFile(join(destination, "keep"), "outside child");
    await materializeRepositoryWorkspace({
      workspace,
      fetch,
      seedUrl: `${fixture.url}/git/${fixture.head}`,
      head: fixture.head,
      branch: "main",
      directory: "source",
    });
    await assert.rejects(readFile(join(destination, "source/partial")), { code: "ENOENT" });
    assert.equal(await readFile(join(destination, "keep"), "utf8"), "outside child");

    await assert.rejects(materializeRepositoryWorkspace({
      workspace,
      fetch,
      seedUrl: `${fixture.url}/git/${fixture.head}`,
      head: fixture.head,
      directory: "../escape",
    }), /safe child name/);
    await assert.rejects(materializeRepositoryWorkspace({
      workspace,
      fetch,
      seedUrl: `${fixture.url}/git/not-pinned`,
      head: fixture.head,
    }), /immutable head/);
  } finally {
    await fixture.close();
    await rm(destination, { recursive: true, force: true });
  }
});

async function gitFixture() {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-git-seed-"));
  const source = join(directory, "source");
  const repositories = join(directory, "repositories");
  await mkdir(source);
  await mkdir(repositories);
  await git(["init", "-q", "-b", "main"], source);
  await git(["config", "user.name", "Nanocodex Test"], source);
  await git(["config", "user.email", "test@nanocodex.dev"], source);
  await writeFile(join(source, "README.md"), "immutable\n");
  await git(["add", "README.md"], source);
  await git(["commit", "-q", "-m", "seed"], source);
  const head = (await git(["rev-parse", "HEAD"], source)).trim();
  await git(["clone", "-q", "--bare", source, join(repositories, "seed.git")], directory);

  const server = createServer(async (request, response) => {
    try {
      const requestUrl = new URL(request.url, "http://localhost");
      const suffix = requestUrl.pathname.match(/^\/git\/[a-f0-9]{40}(\/.*)$/)?.[1];
      if (!suffix) {
        response.writeHead(404).end();
        return;
      }
      const body = [];
      for await (const chunk of request) body.push(chunk);
      const result = await cgi(repositories, `/seed.git${suffix}`, request, requestUrl, body);
      response.writeHead(result.status, result.headers);
      response.end(result.body);
    } catch (error) {
      response.writeHead(500, { "content-type": "text/plain" });
      response.end(String(error));
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  return {
    head,
    url: `http://127.0.0.1:${address.port}`,
    async close() {
      await new Promise((resolve) => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    },
  };
}

async function cgi(projectRoot, pathInfo, request, url, body) {
  const result = await command("git", ["http-backend"], projectRoot, Buffer.concat(body), {
    GIT_PROJECT_ROOT: projectRoot,
    GIT_HTTP_EXPORT_ALL: "1",
    PATH_INFO: pathInfo,
    QUERY_STRING: url.search.slice(1),
    REQUEST_METHOD: request.method,
    CONTENT_TYPE: request.headers["content-type"] ?? "",
    CONTENT_LENGTH: request.headers["content-length"] ?? "0",
    HTTP_GIT_PROTOCOL: request.headers["git-protocol"] ?? "",
  });
  const separator = result.indexOf("\r\n\r\n");
  const header = result.subarray(0, separator).toString("utf8");
  const headers = {};
  let status = 200;
  for (const line of header.split("\r\n")) {
    const colon = line.indexOf(":");
    if (colon === -1) continue;
    const name = line.slice(0, colon);
    const value = line.slice(colon + 1).trim();
    if (name.toLowerCase() === "status") status = Number(value.slice(0, 3));
    else headers[name] = value;
  }
  return { status, headers, body: result.subarray(separator + 4) };
}

function git(args, cwd) {
  return command("git", args, cwd).then((output) => output.toString("utf8"));
}

function command(executable, args, cwd, input, extraEnv = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, { cwd, env: { ...process.env, ...extraEnv } });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", reject);
    child.once("close", (code) => code === 0
      ? resolve(Buffer.concat(stdout))
      : reject(new Error(`${executable} exited ${code}: ${Buffer.concat(stderr)}`)));
    child.stdin.end(input);
  });
}
