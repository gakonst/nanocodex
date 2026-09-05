import { describe, expect, it } from "vitest";

import {
  DEFAULT_MEMORY_SCAN_LIMIT,
  MAX_MEMORY_CONTENT_BYTES,
  MAX_MEMORY_QUERY_BYTES,
  MAX_MEMORY_READ_KEYS,
  MAX_MEMORY_RECORDS,
  MAX_MEMORY_SCAN_RESULTS,
  MAX_MEMORY_TOTAL_CONTENT_BYTES,
  MEMORY_PROBATION_DURATION_MS,
  memoryPreview,
  normalizeMemoryIdentity,
  parseMemoryOperation,
  parseMemoryToolOperation,
  rankMemories,
  tokenizeMemory,
  type MemoryRecord,
} from "../src/durable-memory";

function memory(id: number, content: string): MemoryRecord {
  return {
    key: { id, version: 1 },
    content,
    created_at_ms: 0,
    updated_at_ms: 0,
    last_scanned_at_ms: null,
    scan_count: 0,
    last_used_at_ms: null,
    use_count: 0,
    probation_until_ms: null,
  };
}

describe("durable memory contract", () => {
  it("parses one closed tagged operation surface", () => {
    expect(parseMemoryOperation({ operation: "scan", query: "  rust memory  " })).toEqual({
      operation: "scan",
      query: "  rust memory  ",
      limit: DEFAULT_MEMORY_SCAN_LIMIT,
    });
    expect(parseMemoryOperation({
      operation: "read",
      keys: [{ id: 7, version: 2 }],
    })).toEqual({ operation: "read", keys: [{ id: 7, version: 2 }] });
    expect(parseMemoryOperation({
      operation: "put",
      content: "Prefer invariant-first reviews.",
      replace: { id: 7, version: 2 },
    })).toEqual({
      operation: "put",
      content: "Prefer invariant-first reviews.",
      replace: { id: 7, version: 2 },
    });
    expect(parseMemoryOperation({
      operation: "delete",
      key: { id: 7, version: 3 },
    })).toEqual({ operation: "delete", key: { id: 7, version: 3 } });

    expect(() => parseMemoryOperation({
      operation: "scan",
      query: "rust",
      namespace: "other-account",
    })).toThrow("supported fields for memory scan");
    expect(() => parseMemoryOperation({
      operation: "delete",
      key: { id: 1, version: 1, namespace: "other-account" },
    })).toThrow("supported memory key fields are id and version");
  });

  it("validates positive safe keys and byte-bounded nonempty input", () => {
    for (const key of [
      { id: 0, version: 1 },
      { id: 1, version: -1 },
      { id: 1.5, version: 1 },
      { id: Number.MAX_SAFE_INTEGER + 1, version: 1 },
    ]) {
      expect(() => parseMemoryOperation({ operation: "delete", key })).toThrow(
        "positive safe integers",
      );
    }
    expect(() => parseMemoryOperation({ operation: "scan", query: " \n\t " })).toThrow(
      "query must be non-empty",
    );
    expect(() => parseMemoryOperation({ operation: "put", content: "\u0085\u2003" })).toThrow(
      "content must be non-empty",
    );
    expect(parseMemoryOperation({ operation: "scan", query: "é".repeat(256), limit: 1 }))
      .toMatchObject({ limit: 1 });
    expect(() => parseMemoryOperation({ operation: "scan", query: "é".repeat(257) })).toThrow(
      `${MAX_MEMORY_QUERY_BYTES} UTF-8 bytes`,
    );
    expect(parseMemoryOperation({ operation: "put", content: "é".repeat(512) }))
      .toMatchObject({ operation: "put" });
    expect(() => parseMemoryOperation({ operation: "put", content: "é".repeat(513) })).toThrow(
      `${MAX_MEMORY_CONTENT_BYTES} UTF-8 bytes`,
    );
    for (const limit of [0, 6, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => parseMemoryOperation({ operation: "scan", query: "rust", limit })).toThrow(
        "integer from 1 to 5",
      );
    }
    expect(() => parseMemoryOperation({ operation: "read", keys: [] })).toThrow(
      "requires at least one key",
    );
    expect(() => parseMemoryOperation({
      operation: "read",
      keys: Array.from({ length: MAX_MEMORY_READ_KEYS + 1 }, (_, index) => ({
        id: index + 1,
        version: 1,
      })),
    })).toThrow(`at most ${MAX_MEMORY_READ_KEYS} keys`);
    expect(parseMemoryOperation({
      operation: "read",
      keys: [{ id: 1, version: 1 }, { id: 1, version: 1 }, { id: 1, version: 2 }],
    })).toEqual({
      operation: "read",
      keys: [{ id: 1, version: 1 }, { id: 1, version: 2 }],
    });
  });

  it("bounds oversized model-authored scan limits without relaxing the public parser", () => {
    expect(parseMemoryToolOperation({ operation: "scan", query: "rust", limit: 10 })).toEqual({
      operation: "scan",
      query: "rust",
      limit: MAX_MEMORY_SCAN_RESULTS,
    });
    expect(() => parseMemoryOperation({ operation: "scan", query: "rust", limit: 10 })).toThrow(
      "integer from 1 to 5",
    );
    for (const limit of [0, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => parseMemoryToolOperation({ operation: "scan", query: "rust", limit })).toThrow(
        "integer from 1 to 5",
      );
    }
  });

  it("exports Tact's account bounds and probation duration", () => {
    expect(MAX_MEMORY_RECORDS).toBe(512);
    expect(MAX_MEMORY_TOTAL_CONTENT_BYTES).toBe(256 * 1_024);
    expect(MEMORY_PROBATION_DURATION_MS).toBe(7 * 24 * 60 * 60 * 1_000);
  });
});

describe("durable memory retrieval", () => {
  it("tokenizes paths, underscore identifiers, and camel case exactly like Tact", () => {
    expect(tokenizeMemory("src/core/httpServer.rs parse_request")).toEqual([
      "src",
      "core",
      "httpserver",
      "http",
      "server",
      "rs",
      "parse_request",
      "parse",
      "request",
    ]);
  });

  it("ranks term frequency and document length with deterministic id ties", () => {
    expect(rankMemories("rust sqlite", [
      memory(1, "rust sqlite"),
      memory(2, "rust rust sqlite"),
      memory(3, "rust sqlite unrelated padding words"),
    ]).candidates.map((candidate) => candidate.key.id)).toEqual([2, 1, 3]);

    expect(rankMemories("same", [memory(2, "same"), memory(1, "same")])
      .candidates.map((candidate) => candidate.key.id)).toEqual([1, 2]);
    expect(rankMemories("different", [memory(1, "same")])).toEqual({
      abstained: true,
      candidates: [],
    });
  });

  it("ranks complete and broad partial matches like the pinned reference", () => {
    expect(rankMemories("common rare", [
      memory(1, "common"),
      memory(2, "common rare"),
      memory(3, "common"),
    ]).candidates.map((candidate) => candidate.key.id)).toEqual([2, 1, 3]);

    const candidates = rankMemories(
      "user preferences code review actionable defects read only repository",
      [
        memory(1, "The user prefers invariant-first code review and implementation."),
        memory(2, "The user expects task scope to be followed. Read-only requests authorize no edits."),
        memory(3, "An unrelated repository fact."),
      ],
      2,
    );
    expect(candidates.candidates.map((candidate) => candidate.key.id).sort()).toEqual([1, 2]);
  });

  it("returns UTF-8-safe previews bounded to 64 bytes", () => {
    expect(memoryPreview("Use early returns.")).toBe("Use early returns.");
    expect(memoryPreview("a".repeat(64))).toBe("a".repeat(64));
    expect(memoryPreview("a".repeat(65))).toBe("a".repeat(64));
    expect(memoryPreview(`${"a".repeat(63)}é-tail`)).toBe("a".repeat(63));
    expect(new TextEncoder().encode(memoryPreview("🙂".repeat(17))).byteLength).toBe(64);
  });

  it("normalizes duplicate identity with Unicode whitespace and lowercase joining", () => {
    expect(normalizeMemoryIdentity("  Prefer\tRUST\u0085Reviews  ")).toBe("prefer rust reviews");
  });
});
