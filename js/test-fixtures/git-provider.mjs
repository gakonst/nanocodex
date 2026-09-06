import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

let fixture;
function repository() {
  if (fixture) return fixture;
  const directory = mkdtempSync(join(tmpdir(), "nanocodex-git-egress-"));
  const git = (args, input) => execFileSync("git", ["-C", directory, ...args], {
    input, maxBuffer: Infinity, stdio: ["pipe", "pipe", "pipe"],
  });
  git(["init", "--bare", "--initial-branch=master"]);
  const bytes = randomBytes(17 * 1024 * 1024 + 123);
  const blob = git(["hash-object", "-w", "--stdin"], bytes).toString().trim();
  const tree = git(["mktree"], `100644 blob ${blob}\tlarge.bin\n`).toString().trim();
  const head = git(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
    "commit-tree", tree, "-m", "Large binary repository"]).toString().trim();
  git(["update-ref", "refs/heads/master", head]);
  process.once("exit", () => rmSync(directory, { recursive: true, force: true }));
  fixture = { git, head, size: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") };
  return fixture;
}

/** Native Git smart HTTP fixture: real pack negotiation and incompressible data. */
export async function gitProvider(request) {
  const url = new URL(request.url);
  if (url.href === "https://api.github.com/repos/fixture/large") {
    const { head, size, sha256 } = repository();
    return Response.json({ head, size, sha256 });
  }
  if (url.hostname !== "github.com" || !url.pathname.startsWith("/fixture/large.git/")) return undefined;
  if (request.headers.get("authorization") !== `Basic ${btoa("x-access-token:github-connector-access")}`) {
    return new Response("Authentication required", { status: 401 });
  }
  const { git } = repository();
  if (url.pathname.endsWith("/info/refs") && request.method === "GET") {
    const refs = git(["upload-pack", "--stateless-rpc", "--advertise-refs", "."]);
    return new Response(Buffer.concat([Buffer.from("001e# service=git-upload-pack\n0000"), refs]), {
      headers: { "content-type": "application/x-git-upload-pack-advertisement" },
    });
  }
  if (url.pathname.endsWith("/git-upload-pack") && request.method === "POST") {
    const pack = git(["upload-pack", "--stateless-rpc", "."], Buffer.from(await request.arrayBuffer()));
    return new Response(pack, { headers: { "content-type": "application/x-git-upload-pack-result" } });
  }
  return new Response(null, { status: 405 });
}
