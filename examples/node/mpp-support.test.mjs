import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { delegatedAccessKey, persistentChannelStore } from "./mpp-support.mjs";

test("delegated access-key identity is required and distinct from the root", () => {
  const root = "0x0000000000000000000000000000000000000001";
  const signer = "0x0000000000000000000000000000000000000002";
  assert.equal(delegatedAccessKey(root, { accessKeyAddress: signer }), signer);
  assert.throws(() => delegatedAccessKey(root, {}), /did not select/);
  assert.throws(() => delegatedAccessKey(root, { accessKeyAddress: root }), /root wallet/);
});

test("persistent channel store restores bigint channel state", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-mpp-store-"));
  const file = join(directory, "channels.json");
  const first = persistentChannelStore(file);
  const entry = {
    chainId: 4217,
    channelId: "0x01",
    cumulativeAmount: 7n,
    deposit: 50n,
    descriptor: {
      payee: "0x0000000000000000000000000000000000000001",
      token: "0x0000000000000000000000000000000000000002",
    },
    escrow: "0x0000000000000000000000000000000000000003",
    opened: true,
  };
  await first.set(entry);

  const second = persistentChannelStore(file);
  const restored = await second.get(
    `${entry.descriptor.payee}:${entry.descriptor.token}:${entry.escrow}:${entry.chainId}`,
  );
  assert.equal(restored?.channelId, entry.channelId);
  assert.equal(restored?.cumulativeAmount, 7n);
  assert.equal(restored?.deposit, 50n);
});
