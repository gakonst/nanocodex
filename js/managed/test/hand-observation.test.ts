import { describe, expect, it } from "vitest";
import { screenObservation } from "../src/hand-observation";

const provider = { id: "generic", status: "ok", capturedAt: 1000, freshness: "fresh", data: { text: "state" } };
const wrap = (providers: unknown[] = [provider]) => ({ schemaVersion: 1, capturedAt: 1000, providers });
describe("observation trust boundary", () => {
  it("allowlists metadata and drops artifact carriers from arbitrary provider data", () => {
    const value = screenObservation({ ...wrap([{ ...provider, image_url: "https://bad.test/top", data: {
      text: "safe", nested: { type: "input_image", image_url: "https://bad.test/image", role: "label" },
      mimeType: "image/png", blob: "AA==", ...JSON.parse('{"__proto__":{"polluted":true}}'),
    } }]), extra: "ignored" });
    expect(value).toEqual(wrap([{ ...provider, status: "partial", data: { text: "safe", nested: { role: "label" } } }]));
    expect({}).not.toHaveProperty("polluted");
  });
  it("reports invalid or missing successful data explicitly and preserves provider failures when stripping", () => {
    for (const data of [null, [], "invalid", undefined]) {
      const result = screenObservation(wrap([{ ...provider, data, error: "previous" }]))?.providers[0];
      expect(result).toMatchObject({ status: "error", error: "invalid_provider_data" });
      expect(result).not.toHaveProperty("data");
    }
    const { data: _data, ...withoutData } = provider;
    expect(screenObservation(wrap([withoutData]))?.providers[0]).toMatchObject({ status: "error", error: "invalid_provider_data" });
    for (const status of ["error", "timeout", "unavailable"]) {
      expect(screenObservation(wrap([{ ...withoutData, status, error: "provider_failure" }]))?.providers[0])
        .toEqual({ ...withoutData, status, error: "provider_failure" });
      expect(screenObservation(wrap([{ ...provider, status, error: "provider_failure", data: { type: "image", image_url: "bad", text: "retained" } }]))?.providers[0])
        .toMatchObject({ status, error: "provider_failure", data: { text: "retained" } });
    }
    expect(screenObservation(wrap([{ ...provider, data: { type: "button" } }]))?.providers[0])
      .toMatchObject({ status: "ok", data: { type: "button" } });
  });
  it("ignores malformed envelopes and providers without affecting valid screenshots", () => {
    for (const value of [null, [], {}, { ...wrap(), schemaVersion: 2 }, { ...wrap(), capturedAt: -1 }]) expect(screenObservation(value)).toBeUndefined();
    expect(screenObservation(wrap([null, { ...provider, status: "unknown" }, provider]))?.providers).toEqual([provider]);
  });
  it("bounds provider count, UTF-8 strings, nodes, depth, and serialized data size", () => {
    expect(screenObservation(wrap(Array(20).fill(provider)))?.providers).toHaveLength(5);
    let deep: unknown = "leaf";
    for (let i = 0; i < 10; i++) deep = { nested: deep };
    for (const data of [{ text: "🦄".repeat(129) }, { nodes: Array(2048).fill(1) }, { deep },
      Object.fromEntries(Array.from({ length: 20 }, (_, i) => [String(i), "x".repeat(512)]))]) {
      const result = screenObservation(wrap([{ ...provider, data }]))?.providers[0];
      expect(result).not.toHaveProperty("data");
      expect(result).toMatchObject({ status: "error", error: "invalid_provider_data" });
    }
    expect(screenObservation(wrap([{ ...provider, error: "x".repeat(513), ageMs: -1 }]))?.providers[0]).toEqual(provider);
  });
});
