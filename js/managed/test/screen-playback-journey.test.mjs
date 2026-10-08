// Screen playback journey over real workerd HTTP: production account proxy ->
// managed Worker -> AccountHostedTools broker -> ScreenPlayback DOs. Synthetic
// Hands speak the real host protocol and upload real ffmpeg H.264/AAC segments.
// Only account enrollment, DO eviction and create-race delays are injected.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { appendFile, mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { startPlaybackFixture, SyntheticHost, makeSegments, redact, ownerA, ownerB, root } from "./screen-playback-fixture.mjs";

const command = "corepack pnpm --filter nanocodex-managed-service exec node --test test/screen-playback-journey.test.mjs (Node 24)";
const run = promisify(execFile);

test("screen playback links: create, decode, bounds, eviction/restart recovery, races, revoke, expiry", { timeout: 240_000 }, async () => {
  const output = join(root, "../../output/screen-playback-journey", `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const observed = {}, phases = [];
  const phase = (name, value = {}) => { phases.push({ phase: name, at: Date.now(), ...value }); };
  const fixture = await startPlaybackFixture(join(output, "runtime"));
  const hosts = [];
  try {
    const keyA = await fixture.enroll(ownerA), keyB = await fixture.enroll(ownerB);
    const keyReadOnly = await fixture.enroll(ownerA, ["agents:read", "tools:use"]);
    const http = async (token, method, path, body, headers = {}) => {
      const response = await fetch(new URL(path, fixture.base), { method, redirect: "manual", signal: AbortSignal.timeout(20_000),
        headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers },
        body: body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body) });
      const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
      await appendFile(join(output, "http.jsonl"), redact({ method, path, status: response.status, body: typeof value === "string" ? value.slice(0, 400) : value }) + "\n");
      return { status: response.status, body: value, headers: response.headers };
    };
    const links = (token = keyA) => http(token, "GET", "/v1/account/hands/playback-links");
    const create = (body, token = keyA) => http(token, "POST", "/v1/account/hands/playback-links", { expires_in_seconds: 900, ...body });
    const linkState = async id => (await links()).body.data.find(link => link.id === id);
    const waitFor = async (predicate, description, ms = 20_000) => {
      const deadline = Date.now() + ms;
      while (Date.now() < deadline) { const value = await predicate(); if (value) return value; await delay(100); }
      throw Error(`${description} not observed within ${ms}ms`);
    };
    const view = async (url, init = {}) => {
      const response = await fetch(url, { signal: AbortSignal.timeout(10_000), ...init });
      return { status: response.status, text: init.method === "HEAD" ? "" : await response.text(), headers: response.headers };
    };
    const sequenceOf = text => Number(/#EXT-X-MEDIA-SEQUENCE:(\d+)/.exec(text)?.[1]);
    const decode = async (url, name) => {
      // Real HLS demux/decode by ffmpeg over the public URL; transcript is redacted.
      const { stdout, stderr } = await run("ffmpeg", ["-hide_banner", "-loglevel", "error", "-i", url, "-t", "3", "-map", "0", "-f", "framemd5", "-"],
        { timeout: 60_000, maxBuffer: 16 << 20 });
      const frames = stdout.split("\n").filter(line => /^\d+,/.test(line));
      const video = frames.filter(line => line.startsWith("0,")).length, audio = frames.filter(line => line.startsWith("1,")).length;
      await writeFile(join(output, `${name}-ffmpeg.txt`), redact(`video_frames=${video} audio_packets=${audio}\n${stderr}`));
      return { video, audio };
    };
    const segments = await makeSegments(join(output, "media"), 30);
    const probe = await run("ffprobe", ["-v", "error", "-show_entries", "stream=codec_name", "-of", "csv=p=0", join(output, "media", "g0.ts")]);
    observed.source_codecs = probe.stdout.trim().split("\n");
    const host = options => { const value = new SyntheticHost({ fixture, token: keyA, segments, ...options }); hosts.push(value); return value; };
    const hostA = host({ machineId: "synthetic-playback-a" });
    await hostA.connect();
    phase("host_a_idle", { generation: hostA.generation });

    // Authorization and request bounds.
    observed.auth = {
      unauthenticated: (await http(undefined, "GET", "/v1/account/hands/playback-links")).status,
      read_only_create: (await create({ operation_id: crypto.randomUUID(), machine_id: hostA.machineId, surface_id: "display-1" }, keyReadOnly)).status,
      invalid_operation: (await create({ operation_id: "not-a-uuid", machine_id: hostA.machineId, surface_id: "display-1" })).status,
      oversized_body: (await http(keyA, "POST", "/v1/account/hands/playback-links", JSON.stringify({ operation_id: crypto.randomUUID(), machine_id: "x".repeat(5000), surface_id: "d" }))).status,
      too_long: (await create({ operation_id: crypto.randomUUID(), machine_id: hostA.machineId, surface_id: "display-1", expires_in_seconds: 28801 })).status,
    };
    assert.deepEqual(observed.auth, { unauthenticated: 401, read_only_create: 403, invalid_operation: 400, oversized_body: 400, too_long: 400 });

    // Old Hand (no playback capability) and unknown machine fail before any live claim.
    const hostOld = host({ machineId: "synthetic-old-hand", playback: false });
    await hostOld.connect();
    const old = await create({ operation_id: crypto.randomUUID(), machine_id: hostOld.machineId, surface_id: "display-1" });
    const unknown = await create({ operation_id: crypto.randomUUID(), machine_id: "synthetic-missing-hand", surface_id: "display-1" });
    observed.old_hand = { status: old.status, error: old.body.error, starts_sent: hostOld.starts.length, unknown: unknown.status };
    assert.deepEqual(observed.old_hand, { status: 409, error: "unsupported", starts_sent: 0, unknown: 404 });
    assert.equal((await links()).body.data.length, 0, "rejected creates leave no link");

    // Expiry link (minimum 60 s) starts early and is checked at the end.
    const hostExpiry = host({ machineId: "synthetic-expiry-hand" });
    await hostExpiry.connect();
    const expiring = await create({ operation_id: crypto.randomUUID(), machine_id: hostExpiry.machineId, surface_id: "display-1", expires_in_seconds: 60 });
    assert.equal(expiring.status, 201, JSON.stringify(redact(expiring.body)));

    // Concurrent creates for one Hand: exactly one stream; the other is busy, never a second live claim.
    const opX = crypto.randomUUID(), opY = crypto.randomUUID();
    const [first, second] = await Promise.all([opX, opY].map(operation_id => create({ operation_id, machine_id: hostA.machineId, surface_id: "display-1", generation: hostA.generation })));
    const winner = first.status === 201 ? first : second, loser = first.status === 201 ? second : first;
    observed.concurrent = { statuses: [first.status, second.status].sort(), loser_error: loser.body.error, starts: hostA.starts.length };
    assert.deepEqual(observed.concurrent, { statuses: [201, 409], loser_error: "busy", starts: 1 });
    const winnerOp = winner === first ? opX : opY;
    const link = winner.body, url = new URL(link.url);
    observed.url_shape = { path: url.pathname.replace(link.id, "<id>"), query_keys: [...url.searchParams.keys()], same_origin: url.origin === fixture.base.origin };
    assert.deepEqual(observed.url_shape, { path: "/v1/screen-playback/<id>/index.m3u8", query_keys: ["token"], same_origin: true });
    const replay = await create({ operation_id: winnerOp, machine_id: hostA.machineId, surface_id: "display-1", generation: hostA.generation });
    const conflict = await create({ operation_id: winnerOp, machine_id: hostA.machineId, surface_id: "display-1", preset: "1080p", generation: hostA.generation });
    observed.idempotency = { replay: replay.status, replay_has_url: "url" in replay.body, url_available: replay.body.url_available, same_id: replay.body.id === link.id, conflict: conflict.body.error };
    assert.deepEqual(observed.idempotency, { replay: 200, replay_has_url: false, url_available: false, same_id: true, conflict: "operation_conflict" });
    const listed = await links();
    assert.ok(!JSON.stringify(listed.body).includes("token") && !JSON.stringify(listed.body).includes("url\""), "list never exposes URLs or tokens");
    phase("created", { id: link.id });

    // Live only after accepted media; strict server-rendered playlist; real decode.
    await waitFor(async () => (await linkState(link.id))?.state === "live", "link live");
    const playlist = await waitFor(async () => { const value = await view(url); return value.status === 200 ? value : undefined; }, "playlist");
    const uris = playlist.text.split("\n").filter(line => line && !line.startsWith("#"));
    observed.playlist = {
      content_type: playlist.headers.get("content-type"), cache: playlist.headers.get("cache-control"), referrer: playlist.headers.get("referrer-policy"),
      entries: uris.length, all_relative_token_uris: uris.every(uri => /^s\d+\.ts\?token=nsv_[A-Za-z0-9_-]{43}$/.test(uri) && uri.endsWith(url.search)),
    };
    assert.deepEqual({ ...observed.playlist, entries: observed.playlist.entries <= 6 && observed.playlist.entries > 0 },
      { content_type: "application/vnd.apple.mpegurl", cache: "no-store", referrer: "no-referrer", entries: true, all_relative_token_uris: true });
    observed.decode_live = await decode(url.href, "live");
    assert.ok(observed.decode_live.video >= 20 && observed.decode_live.audio > 0, JSON.stringify(observed.decode_live));
    const head = await view(url, { method: "HEAD" });
    assert.equal(head.status, 200);

    // Wrong or misplaced credentials: uniform 404 for viewers, 401 for uploads.
    const upload = new URL(hostA.upload.url), uploadToken = hostA.upload.token, viewToken = url.searchParams.get("token");
    const withQuery = (path, query) => { const value = new URL(path, url); value.search = query; return value; };
    const put = (file, body, contentType = "video/mp2t", token = uploadToken) => fetch(new URL(file, upload), { method: "PUT",
      headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), "content-type": contentType }, body }).then(async response => {
      const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
      return { status: response.status, body: value };
    });
    const wrongView = "nsv_" + "A".repeat(43);
    observed.credentials = {
      wrong_view_token: (await view(withQuery(url.pathname, `?token=${wrongView}`))).status,
      extra_query: (await view(withQuery(url.pathname, `${url.search}&x=1`))).status,
      path_token: (await view(new URL(`/v1/screen-playback/${link.id}/${viewToken}/index.m3u8`, url))).status,
      upload_token_as_view: (await view(withQuery(url.pathname, `?token=${uploadToken}`))).status,
      view_token_upload: (await put("index.m3u8", "#EXTM3U\n", "application/vnd.apple.mpegurl", viewToken)).status,
      no_auth_upload: (await put("index.m3u8", "#EXTM3U\n", "application/vnd.apple.mpegurl", "")).status,
      other_owner_list: (await links(keyB)).body.data.length,
      other_owner_delete: (await http(keyB, "DELETE", `/v1/account/hands/playback-links/${link.id}`)).status,
    };
    assert.deepEqual(observed.credentials, { wrong_view_token: 404, extra_query: 404, path_token: 404, upload_token_as_view: 404,
      view_token_upload: 401, no_auth_upload: 401, other_owner_list: 0, other_owner_delete: 404 });

    // Bounded RAM window and strict upload validation (none of these advance the accepted sequence).
    await waitFor(() => hostA.next > 9, "ten segments uploaded");
    const ts = Buffer.alloc(188 * 4); for (let offset = 0; offset < ts.length; offset += 188) ts[offset] = 0x47;
    const bad = (lines) => `#EXTM3U\n#EXT-X-TARGETDURATION:2\n${lines}\n`;
    const latest = hostA.next - 1;
    observed.bounds = {
      evicted_old_segment: (await view(withQuery(`/v1/screen-playback/${link.id}/s0.ts`, url.search))).status,
      external_uri: (await put("index.m3u8", bad(`#EXTINF:1.0,\nhttps://example.com/s${latest}.ts`), "application/vnd.apple.mpegurl")).status,
      byte_range: (await put("index.m3u8", bad(`#EXT-X-BYTERANGE:100@0\n#EXTINF:1.0,\ns${latest}.ts`), "application/vnd.apple.mpegurl")).status,
      key_tag: (await put("index.m3u8", bad(`#EXT-X-KEY:METHOD=AES-128,URI="https://example.com/k"\n#EXTINF:1.0,\ns${latest}.ts`), "application/vnd.apple.mpegurl")).status,
      future_segment: (await put("index.m3u8", bad(`#EXTINF:1.0,\ns${latest + 1000}.ts`), "application/vnd.apple.mpegurl")).status,
      seven_entries: (await put("index.m3u8", bad(Array.from({ length: 7 }, (_, i) => `#EXTINF:1.0,\ns${latest - 6 + i}.ts`).join("\n")), "application/vnd.apple.mpegurl")).status,
      playlist_too_large: (await put("index.m3u8", "#EXTM3U\n" + "#".repeat(17 * 1024), "application/vnd.apple.mpegurl")).status,
      not_ts: (await put(`s${latest + 1000}.ts`, Buffer.from("not mpeg-ts"))).status,
      wrong_type: (await put(`s${latest + 1000}.ts`, ts, "application/octet-stream")).status,
      segment_too_large: (await put(`s${latest + 1000}.ts`, Buffer.alloc(4 * 1024 * 1024 + 188, 0x47))).status,
      outside_refill_window: (await put("s0.ts", ts)).body.error,
    };
    assert.deepEqual(observed.bounds, { evicted_old_segment: 404, external_uri: 400, byte_range: 400, key_tag: 400, future_segment: 400,
      seven_entries: 400, playlist_too_large: 413, not_ts: 400, wrong_type: 400, segment_too_large: 413, outside_refill_window: "sequence" });
    const window = await view(url);
    assert.ok(window.text.split("\n").filter(line => line.startsWith("s")).length <= 6);
    assert.equal((await linkState(link.id)).state, "live", "rejected uploads do not disturb the live stream");

    // A different Hand of the same owner cannot change this stream's status.
    const hostForger = host({ machineId: "synthetic-forger-hand" });
    await hostForger.connect();
    hostForger.result(link.id, crypto.randomUUID(), "failed", "capture_failed");
    hostForger.result(link.id, crypto.randomUUID(), "stopped");
    await delay(800);
    observed.forged_result = { state: (await linkState(link.id)).state, view: (await view(url)).status };
    assert.deepEqual(observed.forged_result, { state: "live", view: 200 });
    phase("bounds_and_forgery_checked");

    // DO eviction: RAM window lost, durable sequence kept; the same URL recovers through 409 missing_segments refill.
    const recover = async (label) => {
      const before = sequenceOf((await view(url)).text);
      const refillsBefore = hostA.refills, startsBefore = hostA.starts.length;
      const evicted = await fixture.evict(link.id);
      const immediately = (await view(url)).status;
      await waitFor(() => hostA.refills > refillsBefore, `${label} refill`);
      const after = await waitFor(async () => { const value = await view(url); return value.status === 200 ? value : undefined; }, `${label} playlist`);
      const decoded = await decode(url.href, label);
      return { evicted: evicted.evicted, immediately_after: immediately, refills: hostA.refills - refillsBefore,
        sequence_advanced: sequenceOf(after.text) > before, new_starts: hostA.starts.length - startsBefore,
        state: (await linkState(link.id)).state, decoded: decoded.video >= 20 };
    };
    observed.eviction = await recover("eviction");
    assert.deepEqual({ ...observed.eviction, immediately_after: [200, 503].includes(observed.eviction.immediately_after) },
      { evicted: true, immediately_after: true, refills: observed.eviction.refills, sequence_advanced: true, new_starts: 0, state: "live", decoded: true });
    assert.ok(observed.eviction.refills > 0);
    phase("eviction_recovered", observed.eviction);

    // Whole runtime SIGKILL: every DO restarts from SQLite. The uploader keeps its window; the Hand reconnects with a new generation.
    const refillsBeforeKill = hostA.refills, generationsBefore = hostA.generations.length;
    observed.restart = { signal: await fixture.kill() };
    await delay(500);
    await fixture.start();
    await waitFor(() => hostA.refills > refillsBeforeKill, "restart refill", 30_000);
    const restarted = await waitFor(async () => { const value = await view(url).catch(() => undefined); return value?.status === 200 ? value : undefined; }, "restart playlist", 30_000);
    await waitFor(() => hostA.generations.length > generationsBefore, "host reconnect", 30_000);
    // The reconnected socket has a new generation: its status cannot rewrite a stream bound to the old one.
    hostA.result(link.id, crypto.randomUUID(), "failed", "capture_failed");
    await delay(800);
    observed.restart = { ...observed.restart, playlist: restarted.status, same_id: (await linkState(link.id))?.id === link.id,
      state: (await linkState(link.id)).state, new_generation: hostA.generation !== hostA.generations[0], new_starts: hostA.starts.length - 1,
      decoded: (await decode(url.href, "restart")).video >= 20 };
    assert.deepEqual(observed.restart, { signal: "SIGKILL", playlist: 200, same_id: true, state: "live", new_generation: true, new_starts: 0, decoded: true });
    phase("restart_recovered");

    // Host-reported busy is surfaced as failure, never as live.
    const hostBusy = host({ machineId: "synthetic-busy-hand", mode: "busy" });
    await hostBusy.connect();
    const busy = await create({ operation_id: crypto.randomUUID(), machine_id: hostBusy.machineId, surface_id: "display-1" });
    const busyLink = busy.status === 201 ? busy.body.id : (await links()).body.data.find(entry => entry.machine_id === hostBusy.machineId)?.id;
    const busyState = busyLink ? await waitFor(async () => { const value = await linkState(busyLink); return value?.state === "failed" ? value : undefined; }, "busy failure") : undefined;
    observed.busy = { create: busy.status, error: busy.body.error ?? busyState?.error, state: busyState?.state ?? "none",
      view: busy.status === 201 ? (await view(busy.body.url)).status : 404 };
    assert.ok([201, 409].includes(observed.busy.create));
    assert.deepEqual({ error: observed.busy.error, view: observed.busy.view, live: observed.busy.state === "live" }, { error: "busy", view: 404, live: false });

    // Revoke racing create: (1) between owner reservation and stream init; (2) between host delivery and activation.
    // Revoking an active reservation always sends a (harmless) stop for that stream ID.
    const hostRace = host({ machineId: "synthetic-race-hand" });
    await hostRace.connect();
    const race = async (path) => {
      await fixture.delayNext(path, 1500);
      const startsBefore = hostRace.starts.length, stopsBefore = hostRace.stops;
      const pending = create({ operation_id: crypto.randomUUID(), machine_id: hostRace.machineId, surface_id: "display-1" });
      // Before activation the link may already be live: the Hand can upload as soon as start is delivered.
      const reserved = await waitFor(async () => (await links()).body.data.find(entry => entry.machine_id === hostRace.machineId
        && ["starting", "live"].includes(entry.state)), `${path} reservation`);
      if (path === "/stream/activate") await waitFor(() => hostRace.starts.length > startsBefore, "start delivered");
      const revoked = await http(keyA, "DELETE", `/v1/account/hands/playback-links/${reserved.id}`);
      const created = await pending;
      await delay(500);
      return { revoke: revoked.status, create: created.status, error: created.body.error, url_returned: "url" in created.body,
        state: (await linkState(reserved.id)).state, start_sent: hostRace.starts.length > startsBefore,
        stop_sent: hostRace.stops > stopsBefore, uploader_active: hostRace.active };
    };
    observed.race_before_init = await race("/stream/init");
    assert.deepEqual(observed.race_before_init, { revoke: 200, create: 409, error: "revoked", url_returned: false, state: "revoked",
      start_sent: false, stop_sent: true, uploader_active: false });
    observed.race_before_activate = await race("/stream/activate");
    assert.deepEqual(observed.race_before_activate, { revoke: 200, create: 409, error: "revoked", url_returned: false, state: "revoked",
      start_sent: true, stop_sent: true, uploader_active: false });

    // Revoke: viewer 404, uploader 410 -> stops; idempotent; unknown 404.
    const stopsBefore = hostA.stops;
    const revoked = await http(keyA, "DELETE", `/v1/account/hands/playback-links/${link.id}`);
    const again = await http(keyA, "DELETE", `/v1/account/hands/playback-links/${link.id}`);
    await waitFor(() => !hostA.active, "uploader stopped");
    observed.revoke = { status: revoked.status, state: revoked.body.state, idempotent: again.status === 200 && again.body.revoked_at === revoked.body.revoked_at,
      view: (await view(url)).status, segment_view: (await view(withQuery(`/v1/screen-playback/${link.id}/s${hostA.next - 1}.ts`, url.search))).status,
      upload: (await put(`s${hostA.next + 5}.ts`, ts)).status, host_stop_sent: hostA.stops > stopsBefore,
      unknown: (await http(keyA, "DELETE", `/v1/account/hands/playback-links/sp_${"0".repeat(32)}`)).status, listed: (await linkState(link.id)).state };
    assert.deepEqual(observed.revoke, { status: 200, state: "revoked", idempotent: true, view: 404, segment_view: 404, upload: 410,
      host_stop_sent: true, unknown: 404, listed: "revoked" });

    // Expiry: uniform 404 to viewers, 410 to the uploader (which stops), listed as expired.
    const expiryUrl = expiring.body.url;
    assert.equal((await view(expiryUrl)).status, 200, "expiring link is live before expiry");
    await waitFor(() => Date.now() > expiring.body.expires_at + 500, "expiry time", 90_000);
    await waitFor(() => !hostExpiry.active, "expired uploader stopped", 10_000);
    observed.expiry = { view: (await view(expiryUrl)).status, listed: (await linkState(expiring.body.id)).state,
      uploader_stop: hostExpiry.events.find(event => event.type === "uploader_stopped")?.status };
    assert.deepEqual(observed.expiry, { view: 404, listed: "expired", uploader_stop: 410 });
    phase("done");
  } finally {
    for (const value of hosts) value.close();
    await fixture.stop().catch(() => undefined);
    await writeFile(join(output, "hosts.json"), redact(JSON.stringify(hosts.map(value => ({ machine: value.machineId, starts: value.starts,
      stops: value.stops, refills: value.refills, generations: value.generations.length, events: value.events })), null, 2)));
    await writeFile(join(output, "result.json"), redact(JSON.stringify({ command, observed, phases }, null, 2)));
  }
});
