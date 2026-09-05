import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  assertCachedManagedWasmAttestation,
  hashManagedWasmArtifacts,
} from "./check-managed-wasm.mjs";

const packageDirectory = new URL("../pkg-web/", import.meta.url);
const repositoryDirectory = fileURLToPath(new URL("../../../", import.meta.url));
const checkCache = process.argv[2] === "--check-cache";
const sourceWasmArgument = process.argv[checkCache ? 3 : 2];
if (process.argv.length !== (checkCache ? 4 : 3) || !sourceWasmArgument) {
  throw new Error("WASM attestation requires exactly one source WASM path");
}
const sourceWasmPath = resolve(sourceWasmArgument);

const revision = git("rev-parse", "HEAD");
const dirty = git("status", "--porcelain", "--untracked-files=no").length > 0;
const sourceWasmSha256 = createHash("sha256")
  .update(await readFile(sourceWasmPath))
  .digest("hex");
const artifacts = await hashManagedWasmArtifacts(packageDirectory);
const attestation = { schema: 1, revision, dirty, sourceWasmSha256, artifacts };

const attestationUrl = new URL("nanocodex-build.json", packageDirectory);
if (checkCache) {
  const retained = JSON.parse(await readFile(attestationUrl, "utf8"));
  assertCachedManagedWasmAttestation(retained, { artifacts, sourceWasmSha256 });
} else {
  const temporaryUrl = new URL(`.nanocodex-build-${randomUUID()}.tmp`, packageDirectory);
  try {
    await writeFile(temporaryUrl, `${JSON.stringify(attestation)}\n`, { flag: "wx" });
    await rename(temporaryUrl, attestationUrl);
  } finally {
    await rm(temporaryUrl, { force: true });
  }
}

function git(...arguments_) {
  return execFileSync("git", arguments_, {
    cwd: repositoryDirectory,
    encoding: "utf8",
  }).trim();
}
