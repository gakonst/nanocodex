import assert from "node:assert/strict";
import test from "node:test";
import { RemoteStatsSampler } from "./handRemoteStats.ts";

const video = (overrides: Record<string, unknown> = {}) => ({
  id: "video", type: "inbound-rtp", timestamp: 1000, kind: "video", trackIdentifier: "picture",
  framesDecoded: 1000, bytesReceived: 1_000_000, totalDecodeTime: 20,
  jitterBufferDelay: 100, jitterBufferEmittedCount: 1000, framesDropped: 100,
  frameWidth: 1920, frameHeight: 1080, codecId: "codec", transportId: "transport", ...overrides,
});
function report(overrides: Record<string, unknown> = {}) {
  return new Map<string, { id: string; type: string; timestamp: number; [key: string]: unknown }>([
    ["video", video(overrides)],
    ["audio", { id: "audio", type: "inbound-rtp", timestamp: 1000, kind: "audio", framesDecoded: 9000 }],
    ["codec", { id: "codec", type: "codec", timestamp: 1000, mimeType: "video/H264" }],
    ["transport", { id: "transport", type: "transport", timestamp: 1000, selectedCandidatePairId: "pair" }],
    ["pair", { id: "pair", type: "candidate-pair", timestamp: 1000, currentRoundTripTime: .025 }],
    ["old-pair", { id: "old-pair", type: "candidate-pair", timestamp: 1000, currentRoundTripTime: 9, nominated: true, state: "succeeded" }],
  ]);
}

test("stats use interval counters and report timestamps, not lifetime averages or stale native FPS", () => {
  const sampler = new RemoteStatsSampler();
  const initial = sampler.sample(report(), "picture");
  assert.equal(initial.decodeFps, undefined);
  assert.equal(initial.decodeMs, undefined);
  assert.equal(initial.roundTripMs, 25, "only the selected candidate path provides RTT");
  assert.equal(initial.codec, "H264");
  assert.equal(initial.width, 1920); assert.equal(initial.height, 1080);
  const stats = sampler.sample(report({ timestamp: 3000, framesDecoded: 1040, bytesReceived: 1_500_000,
    totalDecodeTime: 20.2, jitterBufferDelay: 100.8, jitterBufferEmittedCount: 1040, framesDropped: 103,
    framesPerSecond: 60 }), "picture");
  assert.equal(stats.decodeFps, 20);
  assert.equal(stats.bitrateKbps, 2000);
  assert.ok(Math.abs(stats.decodeMs! - 5) < 1e-8);
  assert.ok(Math.abs(stats.jitterBufferMs! - 20) < 1e-8);
  assert.equal(stats.droppedFrames, 3);
});

test("a stopped video reports zero interval FPS and bitrate without inventing per-frame delays", () => {
  const sampler = new RemoteStatsSampler(); sampler.sample(report());
  const stats = sampler.sample(report({ timestamp: 2000, framesPerSecond: 60 }));
  assert.equal(stats.decodeFps, 0); assert.equal(stats.bitrateKbps, 0);
  assert.equal(stats.decodeMs, undefined); assert.equal(stats.jitterBufferMs, undefined); assert.equal(stats.droppedFrames, 0);
});

test("replacement streams, counter resets and nonadvancing clocks do not create invalid rates", () => {
  const sampler = new RemoteStatsSampler(); sampler.sample(report());
  assert.equal(sampler.sample(report({ timestamp: 2000, id: "replacement", framesDecoded: 4000 })).decodeFps, undefined);
  assert.equal(sampler.sample(report({ timestamp: 3000, id: "replacement", framesDecoded: 2, bytesReceived: 10,
    framesDropped: 0, jitterBufferDelay: 0, totalDecodeTime: 0 })).decodeFps, undefined);
  const stats = sampler.sample(report({ timestamp: 3000, id: "replacement", framesDecoded: 20 }));
  assert.equal(stats.decodeFps, undefined); assert.equal(stats.bitrateKbps, undefined);
  assert.equal(sampler.sample(report({ timestamp: 2000, id: "replacement" })).decodeFps, undefined);
});

test("unsupported or malformed counters stay unavailable, including ambiguous or retired tracks", () => {
  const sampler = new RemoteStatsSampler(); sampler.sample(report());
  const stats = sampler.sample(report({ timestamp: 2000, framesDecoded: NaN, bytesReceived: Infinity,
    framesDropped: -1, totalDecodeTime: undefined, jitterBufferDelay: undefined }));
  assert.equal(stats.decodeFps, undefined); assert.equal(stats.bitrateKbps, undefined);
  assert.equal(stats.decodeMs, undefined); assert.equal(stats.jitterBufferMs, undefined); assert.equal(stats.droppedFrames, undefined);
  assert.deepEqual(sampler.sample(report(), "retired"), {});
  const multiple = report(); multiple.set("other", video({ id: "other", trackIdentifier: "other" }));
  assert.deepEqual(sampler.sample(multiple), {});
  assert.equal(sampler.sample(multiple, "other").width, 1920);
  assert.deepEqual(sampler.sample(new Map()), {});
  assert.equal(sampler.sample(report()).decodeFps, undefined, "missing video discards the prior baseline");
});

test("legacy mediaType and unique nominated pair work without exposing candidate addresses", () => {
  const sampler = new RemoteStatsSampler();
  const legacy = report({ kind: undefined, mediaType: "video", trackIdentifier: undefined, transportId: undefined });
  legacy.delete("pair");
  assert.equal(sampler.sample(legacy, "picture").roundTripMs, 9000);
  legacy.set("another", { id: "another", type: "candidate-pair", timestamp: 1000, nominated: true, state: "succeeded", currentRoundTripTime: .01 });
  assert.equal(sampler.sample(legacy).roundTripMs, undefined, "never guess between several nominated paths");
});


test("path diagnostics expose only selected candidate enums, never network addresses or credentials", () => {
  const sample = report();
  sample.set("pair", { ...sample.get("pair")!, localCandidateId: "local", remoteCandidateId: "remote" });
  sample.set("local", { id: "local", type: "local-candidate", timestamp: 1000,
    candidateType: "relay", protocol: "udp", relayProtocol: "tls", address: "private-address",
    url: "turn:private-endpoint", usernameFragment: "private-credential", port: 12345 });
  sample.set("remote", { id: "remote", type: "remote-candidate", timestamp: 1000,
    candidateType: "prflx", protocol: "udp", address: "private-peer-address" });
  const stats = new RemoteStatsSampler().sample(sample);
  assert.equal(stats.localCandidateType, "relay");
  assert.equal(stats.remoteCandidateType, "prflx");
  assert.equal(stats.candidateProtocol, "udp");
  assert.equal(stats.relayProtocol, "tls");
  assert.equal(stats.roundTripMs, 25);
  assert.equal(JSON.stringify(stats).includes("private"), false);
});

test("unknown candidate fields cannot escape the allowlist or label a direct pair as TURN", () => {
  const sample = report();
  sample.set("pair", { ...sample.get("pair")!, localCandidateId: "local", remoteCandidateId: "remote" });
  sample.set("local", { id: "local", type: "local-candidate", timestamp: 1000,
    candidateType: "host", protocol: "private-address", relayProtocol: "tls" });
  sample.set("remote", { id: "remote", type: "remote-candidate", timestamp: 1000,
    candidateType: "private-credential" });
  const stats = new RemoteStatsSampler().sample(sample);
  assert.equal(stats.localCandidateType, "host");
  assert.equal(stats.remoteCandidateType, undefined);
  assert.equal(stats.candidateProtocol, undefined);
  assert.equal(stats.relayProtocol, undefined);
  assert.equal(JSON.stringify(stats).includes("private"), false);
});


test("an unavailable selected pair cannot fall back to a retired or unrelated path", () => {
  const sampler = new RemoteStatsSampler(), sample = report();
  sample.delete("pair");
  assert.equal(sampler.sample(sample).roundTripMs, undefined, "explicit selection takes precedence over an old nominated pair");
  sample.set("transport", { id: "transport", type: "transport", timestamp: 1000 });
  sample.set("old-pair", { ...sample.get("old-pair")!, transportId: "another-transport" });
  assert.equal(sampler.sample(sample).roundTripMs, undefined, "another transport is not the video path");
});
