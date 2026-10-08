// Executed inside screen-playback-fixture.mjs -- with a synthetic account.
// Invokes the shipped CLI, then reads the actual public playback URL over HTTP.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
const binary = process.argv[2];
if (!binary || !process.env.SCREEN_PLAYBACK_TEST_ORIGIN) throw Error("Run inside the playback fixture with a built CLI path.");
const run = promisify(execFile);
const invoke = async (...args) => {
  try {
    const { stdout } = await run(binary, ["hand", "stream", ...args], { timeout: 20_000, maxBuffer: 1 << 20 });
    return JSON.parse(stdout.trim());
  } catch { throw Error(`CLI hand stream ${args[0]} failed; no credential-bearing subprocess output retained.`); }
};
let id;
try {
  const receipt = await invoke("create", "synthetic-playback-hand", "--expires-in", "60", "--preset", "720p");
  id = receipt.id;
  assert.match(id, /^sp_[0-9a-f]{32}$/);
  const url = new URL(receipt.url);
  assert.equal(url.origin, process.env.SCREEN_PLAYBACK_TEST_ORIGIN);
  assert.equal(url.pathname, `/v1/screen-playback/${id}/index.m3u8`);
  assert.equal([...url.searchParams].length, 1);
  assert.equal((await invoke("list")).data.some(link => link.id === id && ["starting", "live"].includes(link.state)), true);
  let playlist;
  for (let n = 0; n < 50; n++) {
    const response = await fetch(url, { signal: AbortSignal.timeout(3000) });
    if (response.status === 200) { playlist = await response.text(); break; }
    await response.body?.cancel();
    await delay(200);
  }
  assert.ok(playlist?.startsWith("#EXTM3U"), "playlist became available");
  const uri = playlist.split("\n").find(line => line.startsWith("s"));
  assert.ok(uri, "playlist names a segment");
  const segment = await fetch(new URL(uri, url), { signal: AbortSignal.timeout(3000) });
  assert.equal(segment.status, 200);
  const bytes = new Uint8Array(await segment.arrayBuffer());
  assert.ok(bytes.length > 188 && bytes.length % 188 === 0 && bytes[0] === 0x47, "public MPEG-TS media");
  const stopped = await invoke("stop", id);
  assert.equal(stopped.state, "revoked");
  const denied = await fetch(url, { signal: AbortSignal.timeout(3000) });
  assert.equal(denied.status, 404);
  await denied.body?.cancel();
  assert.equal((await invoke("list")).data.find(link => link.id === id)?.state, "revoked");
  console.log(JSON.stringify({ result: "PASS", journey: "shipped CLI create/list/play/stop", media_bytes: bytes.length }));
} finally {
  if (id) await invoke("stop", id).catch(() => {});
}
