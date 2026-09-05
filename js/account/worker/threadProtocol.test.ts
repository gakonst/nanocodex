import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildLsRefsResponse,
  buildReceiveReport,
  encodePacketLine,
  legacyRepositoryAdvertisement,
  parsePacketLines,
  parseReceiveRequest,
  parseV2Command,
  receiveAdvertisement,
  repositoryAdvertisement,
} from "./threadProtocol.ts";
import type { ThreadRepository } from "./threadRepository.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const head = "a".repeat(40);
const zero = "0".repeat(40);

test("upload-pack advertises protocol v2 and an empty repository has no refs", () => {
  const advertisement = dataLines(repositoryAdvertisement());
  assert.deepEqual(advertisement, [
    "version 2\n",
    "agent=nanocodex-cloudflare/1\n",
    "ls-refs=unborn\n",
    "fetch\n",
    "object-format=sha1\n",
  ]);
  assert.deepEqual(parsePacketLines(buildLsRefsResponse(undefined, [])), [{ kind: "flush" }]);
});

test("receive-pack advertises empty and persisted thread refs", () => {
  const lines = dataLines(receiveAdvertisement());
  assert.equal(lines[0], "# service=git-receive-pack\n");
  assert.match(lines[1]!, new RegExp(`^${zero} capabilities\\^\\{\\}\\0report-status`));
  const persisted = dataLines(receiveAdvertisement(repository()));
  assert.equal(persisted[0], "# service=git-receive-pack\n");
  assert.match(persisted[1]!, new RegExp(`^${head} refs/heads/nanocodex\\0report-status`));
});

test("protocol v0 advertises the persisted branch for browser Git clients", () => {
  const advertisement = dataLines(legacyRepositoryAdvertisement(repository()));
  assert.equal(advertisement[0], "# service=git-upload-pack\n");
  assert.match(advertisement[1]!, new RegExp(`^${head} HEAD\\0.*symref=HEAD:refs/heads/nanocodex`));
  assert.equal(advertisement[2], `${head} refs/heads/nanocodex\n`);
});

test("receive-pack splits commands from the raw pack body", () => {
  const pack = encoder.encode("PACK-and-the-rest");
  const body = concatenate([
    encodePacketLine(`${zero} ${head} refs/heads/nanocodex\0 report-status side-band-64k ofs-delta\n`),
    encoder.encode("0000"),
    pack,
  ]);
  assert.deepEqual(parseReceiveRequest(body), {
    commands: [{ oldOid: zero, newOid: head, ref: "refs/heads/nanocodex" }],
    pack,
    reportStatus: true,
    sideBand64k: true,
  });
  assert.deepEqual(dataLines(buildReceiveReport({
    oldOid: zero,
    newOid: head,
    ref: "refs/heads/nanocodex",
  })), ["unpack ok\n", "ok refs/heads/nanocodex\n"]);
});

test("ls-refs exposes the thread branch and HEAD symref", () => {
  assert.deepEqual(dataLines(buildLsRefsResponse(repository(), ["ref-prefix refs/heads/"])), [
    `${head} HEAD symref-target:refs/heads/nanocodex\n`,
    `${head} refs/heads/nanocodex\n`,
  ]);
});

function repository(): ThreadRepository {
  return {
    version: 1,
    branch: "nanocodex",
    head,
    refs: [{ name: "refs/heads/nanocodex", oid: head }],
    packs: [{
      key: "thread-repositories/thread-123/pack.pack",
      hash: "b".repeat(40),
      size: 123,
      objectCount: 3,
      oldOid: "0".repeat(40),
      newOid: head,
    }],
    updatedAt: "2026-08-18T00:00:00.000Z",
  };
}

test("packet parsing rejects truncation and parses protocol v2 sections", () => {
  const body = concatenate([
    encodePacketLine("command=fetch\n"),
    encoder.encode("0001"),
    encodePacketLine(`want ${head}\n`),
    encodePacketLine("done\n"),
    encoder.encode("0000"),
  ]);
  assert.deepEqual(parseV2Command(body), {
    command: "fetch",
    arguments: [`want ${head}`, "done"],
  });
  assert.throws(() => parsePacketLines(encoder.encode("0008abc")), /truncated/);
});

function dataLines(bytes: Uint8Array): string[] {
  return parsePacketLines(bytes)
    .filter((packet) => packet.kind === "data")
    .map((packet) => decoder.decode(packet.data));
}

function concatenate(chunks: readonly Uint8Array[]): Uint8Array {
  const result = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}
