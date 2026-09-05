import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildFullPackResponse,
  buildLsRefsResponse,
  encodePacketLine,
  parseFetchArguments,
  parsePacketLines,
  parseV2Command,
  repositoryAdvertisement,
} from "./gitProtocol.ts";
import type { RepositoryPublication } from "./gitRepository.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const hash = "a".repeat(40);
const packHash = "c".repeat(40);

const publication: RepositoryPublication = {
  version: 1,
  head: hash,
  branch: "master",
  refs: [
    { name: "refs/heads/master", oid: hash },
    { name: "refs/tags/v1", oid: "b".repeat(40), peeled: hash },
  ],
  snapshotKey: `generations/${hash}/repository.json`,
  commitsKey: `generations/${hash}/commits.json`,
  commitPatchParts: [{
    key: `generations/${hash}/commit-patches/0000.diff`,
    size: 1,
  }],
  commitPatchSize: 1,
  inventoryKey: `generations/${hash}/inventory.json`,
  packParts: [{ key: `generations/${hash}/packs/${packHash}/0000.pack`, size: 1 }],
  packSize: 1,
  objectManifestKey: `generations/${hash}/objects.json`,
  packHash,
  publishedAt: "2026-08-17T00:00:00.000Z",
};

test("packet-line framing rejects truncation and parses command sections", () => {
  const body = concatenate([
    encodePacketLine("command=fetch\n"),
    encodePacketLine("agent=git/2.0\n"),
    encoder.encode("0001"),
    encodePacketLine(`want ${hash}\n`),
    encodePacketLine("done\n"),
    encoder.encode("0000"),
  ]);
  assert.deepEqual(parseV2Command(body), {
    command: "fetch",
    arguments: [`want ${hash}`, "done"],
  });
  assert.throws(() => parsePacketLines(encoder.encode("0008abc")), /truncated/);
  assert.throws(() => parsePacketLines(encoder.encode("zzzz")), /invalid/);
});

test("fetch arguments preserve negotiation and shallow state", () => {
  assert.deepEqual(parseFetchArguments([
    `want ${hash}`,
    `have ${"b".repeat(40)}`,
    `shallow ${"c".repeat(40)}`,
    "deepen 3",
    "done",
  ]), {
    wants: [hash],
    haves: ["b".repeat(40)],
    shallow: ["c".repeat(40)],
    deepen: 3,
    deepenRelative: false,
    done: true,
  });
  assert.equal(parseFetchArguments(["deepen-relative"]).deepenRelative, true);
});

test("advertisement and ls-refs expose only the published generation", () => {
  const advertisement = parsePacketLines(repositoryAdvertisement())
    .filter((packet) => packet.kind === "data")
    .map((packet) => decoder.decode(packet.data));
  assert.deepEqual(advertisement, [
    "version 2\n",
    "agent=nanocodex-cloudflare/1\n",
    "ls-refs=unborn\n",
    "fetch=shallow\n",
    "object-format=sha1\n",
  ]);

  const refs = parsePacketLines(buildLsRefsResponse(publication, ["ref-prefix refs/heads/"]))
    .filter((packet) => packet.kind === "data")
    .map((packet) => decoder.decode(packet.data));
  assert.deepEqual(refs, [
    `${hash} HEAD symref-target:refs/heads/master\n`,
    `${hash} refs/heads/master\n`,
  ]);

  const tags = parsePacketLines(buildLsRefsResponse(publication, ["peel", "ref-prefix refs/tags/"]))
    .filter((packet) => packet.kind === "data")
    .map((packet) => decoder.decode(packet.data));
  assert.deepEqual(tags, [
    `${hash} HEAD symref-target:refs/heads/master\n`,
    `${"b".repeat(40)} refs/tags/v1 peeled:${hash}\n`,
  ]);
});

test("full-pack responses preserve every pack byte across sideband packets", async () => {
  const pack = new Uint8Array(140_000);
  for (let index = 0; index < pack.length; index++) pack[index] = index % 251;
  const response = buildFullPackResponse(new Blob([pack]).stream());
  const framed = new Uint8Array(await new Response(response).arrayBuffer());
  const packets = parsePacketLines(framed);
  assert.equal(decoder.decode(packets[0]?.kind === "data" ? packets[0].data : new Uint8Array()), "packfile\n");
  const packChunks = packets.slice(1)
    .filter((packet) => packet.kind === "data")
    .map((packet) => {
      assert.equal(packet.data[0], 1);
      return packet.data.subarray(1);
    });
  assert.deepEqual(concatenate(packChunks), pack);
  assert.equal(packets.at(-1)?.kind, "flush");
});

function concatenate(chunks: readonly Uint8Array[]): Uint8Array {
  const result = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}
