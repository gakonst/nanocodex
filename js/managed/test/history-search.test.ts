import { describe, expect, it } from "vitest";

import {
  HISTORY_VECTOR_MATCH_THRESHOLD,
  historyFtsQuery,
  historySearchTerms,
  historyVectorRetrieval,
  isAcceptedHistoryLexicalMatch,
  isExactHistoryIdentifierQuery,
  parseHistoryFindSessionsInput,
  parseHistoryReadSessionInput,
} from "../src/history-search";

describe("history search query routing", () => {
  it("keeps exact identifier queries on authoritative lexical search", () => {
    expect(isExactHistoryIdentifierQuery("MEMORY_STRESS_A11_T1")).toBe(true);
    expect(isExactHistoryIdentifierQuery("release:artifact_42")).toBe(true);
    expect(isExactHistoryIdentifierQuery("  MEMORY_STRESS_A11_T1  ")).toBe(true);
  });

  it("keeps prose and ordinary hyphenated terms eligible for vector search", () => {
    expect(isExactHistoryIdentifierQuery("gemstone waterfowl designation")).toBe(false);
    expect(isExactHistoryIdentifierQuery("memory-scope architecture")).toBe(false);
    expect(isExactHistoryIdentifierQuery("A11 T1")).toBe(false);
  });

  it("configures provider-side vector rejection before result limiting", () => {
    expect(historyVectorRetrieval("organization-a", "team-a", 8)).toEqual({
      retrieval_type: "vector",
      match_threshold: HISTORY_VECTOR_MATCH_THRESHOLD,
      max_num_results: 24,
      filters: {
        organization_id: { $eq: "organization-a" },
        team_id: { $eq: "team-a" },
      },
      return_on_failure: false,
    });
    expect(HISTORY_VECTOR_MATCH_THRESHOLD).toBe(0.5);
  });

  it("requires lexical coverage instead of accepting one crowded-in term", () => {
    expect(historySearchTerms("What is the Atlas cargo insurance policy number?")).toEqual([
      "atlas", "cargo", "insurance", "policy", "number",
    ]);
    expect(historyFtsQuery("What is the Atlas cargo insurance policy number?")).toBe(
      '"atlas" OR "cargo" OR "insurance" OR "policy" OR "number"',
    );
    expect(isAcceptedHistoryLexicalMatch(
      "What is the Atlas cargo insurance policy number?",
      "The Atlas schedule changed.",
    )).toBe(false);
    expect(isAcceptedHistoryLexicalMatch(
      "What is the Atlas cargo insurance policy number?",
      "Atlas insurance policy 42 covers the shipment.",
    )).toBe(true);
    expect(isAcceptedHistoryLexicalMatch("copper lighthouse", "copper only")).toBe(false);
    expect(isAcceptedHistoryLexicalMatch("copper lighthouse", "the copper lighthouse")).toBe(true);
    expect(isAcceptedHistoryLexicalMatch(
      "silver otter",
      "Otter habitats use silver ankle tags.",
    )).toBe(false);
    expect(isAcceptedHistoryLexicalMatch("what was it", "what was it")).toBe(false);
  });

  it("keeps exact identifiers eligible as a single lexical term", () => {
    expect(historyFtsQuery("COPPER_LIGHTHOUSE_MEMORY")).toBe('"copper_lighthouse_memory"');
    expect(isAcceptedHistoryLexicalMatch(
      "COPPER_LIGHTHOUSE_MEMORY",
      "Assistant: COPPER_LIGHTHOUSE_MEMORY",
    )).toBe(true);
    expect(isAcceptedHistoryLexicalMatch(
      "release:artifact_42",
      "Published release:artifact_42 after verification.",
    )).toBe(true);
    expect(isAcceptedHistoryLexicalMatch(
      "release:artifact_42",
      "Published release:artifact_42. Verification followed.",
    )).toBe(true);
    expect(isAcceptedHistoryLexicalMatch(
      "release:artifact_42",
      "Published release:artifact_42.json after verification.",
    )).toBe(false);
    expect(isAcceptedHistoryLexicalMatch(
      "release:artifact_42",
      "Published release artifact_42 after verification.",
    )).toBe(false);
  });

  it("validates the public find_sessions and read_session contracts", () => {
    expect(parseHistoryFindSessionsInput({ query: "  copper lighthouse ", limit: 4 })).toEqual({
      query: "copper lighthouse",
      limit: 4,
    });
    expect(parseHistoryReadSessionInput({
      session_id: "018f1f9a-7b3c-7a09-8000-000000000009",
      turn_ids: ["turn-1", "turn:2"],
    })).toEqual({
      session_id: "018f1f9a-7b3c-7a09-8000-000000000009",
      turn_ids: ["turn-1", "turn:2"],
    });
    expect(() => parseHistoryFindSessionsInput({
      query: "copper",
      limit: 4,
      agentic: false,
    })).toThrow("supported fields are query and limit");
    expect(() => parseHistoryReadSessionInput({
      session_id: "not-a-session",
    })).toThrow("invalid session id");
  });
});
