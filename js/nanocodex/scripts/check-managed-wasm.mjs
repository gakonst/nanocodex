import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultPackageDirectory = fileURLToPath(new URL("../pkg-web/", import.meta.url));
export const managedWasmArtifactNames = [
  "nanocodex.js",
  "nanocodex.d.ts",
  "nanocodex_bg.js",
  "nanocodex_bg.wasm",
  "nanocodex_worker.js",
  "package.json",
];

export function assertCachedManagedWasmAttestation(attestation, {
  artifacts,
  sourceWasmSha256,
}) {
  assert.match(
    attestation?.sourceWasmSha256 ?? "",
    /^[0-9a-f]{64}$/,
    "managed WASM source digest must be SHA-256",
  );
  assert.match(
    attestation?.revision ?? "",
    /^[0-9a-f]{40}$/,
    "managed WASM build revision must be a full commit SHA",
  );
  assert.deepEqual(
    Object.keys(attestation).sort(),
    ["artifacts", "dirty", "revision", "schema", "sourceWasmSha256"],
    "managed WASM attestation schema must be exact",
  );
  assert.equal(attestation.schema, 1, "managed WASM attestation schema must be current");
  assert.equal(typeof attestation.dirty, "boolean", "managed WASM dirty state must be explicit");
  assert.equal(
    attestation.sourceWasmSha256,
    sourceWasmSha256,
    "managed WASM source bytes must match their build attestation",
  );
  assert.deepEqual(
    attestation.artifacts,
    artifacts,
    "managed WASM artifact bytes must match their build attestation",
  );
}

export function assertManagedWasmAttestation(attestation, revision, artifacts) {
  assert.match(revision, /^[0-9a-f]{40}$/, "managed WASM revision must be a full commit SHA");
  assertCachedManagedWasmAttestation(attestation, {
    artifacts,
    sourceWasmSha256: attestation?.sourceWasmSha256,
  });
  assert.equal(
    attestation.revision,
    revision,
    "managed WASM artifacts must be built from the exact production revision",
  );
  assert.equal(attestation.dirty, false, "managed WASM artifacts must be built from clean source");
}

export async function verifyManagedWasmArtifact(revision, {
  packageDirectory = defaultPackageDirectory,
} = {}) {
  const directory = pathToFileURL(`${resolve(packageDirectory)}/`);
  let attestation;
  let artifacts;
  try {
    [attestation, artifacts] = await Promise.all([
      readFile(new URL("nanocodex-build.json", directory), "utf8").then(JSON.parse),
      hashManagedWasmArtifacts(directory),
    ]);
  } catch (error) {
    throw new Error("managed WASM build attestation is missing or invalid", { cause: error });
  }
  assertManagedWasmAttestation(attestation, revision, artifacts);

  const wasm = await readFile(new URL("nanocodex_bg.wasm", directory));
  const glue = await import(new URL(`nanocodex.js?attestation=${artifacts["nanocodex.js"]}`, directory));
  await glue.default({ module_or_path: wasm });
  await assert.rejects(
    glue.Nanocodex.create(JSON.stringify({
      api_key: "production-abi-check",
      host_definition_id: 0,
      durability_id: "production-abi-check",
      durability_host_id: "production-abi-check",
    })),
    /host_definition_id must be at least 1/,
    "managed WASM must accept the complete current durability configuration",
  );

  return { artifacts, revision };
}

export async function hashManagedWasmArtifacts(packageDirectory) {
  return Object.fromEntries(await Promise.all(managedWasmArtifactNames.map(async (name) => [
    name,
    createHash("sha256").update(await readFile(new URL(name, packageDirectory))).digest("hex"),
  ])));
}

const invoked = process.argv[1]
  ? pathToFileURL(resolve(process.argv[1])).href
  : undefined;
if (invoked === import.meta.url) {
  if (process.argv.length !== 3) {
    throw new Error("managed WASM verification requires exactly one production revision");
  }
  const result = await verifyManagedWasmArtifact(process.argv[2]);
  process.stdout.write(`${JSON.stringify({
    revision: result.revision,
    status: "verified",
  })}\n`);
}
