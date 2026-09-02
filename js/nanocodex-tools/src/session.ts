export type SessionPromptInput = string | readonly (
  | Readonly<{ type: "text"; text: string }>
  | Readonly<{ type: "image"; imageUrl?: string }>
  | Readonly<{ type: "audio"; audioUrl?: string }>
)[];

const DEFAULT_LIMIT = 8;
export const MAX_HISTORY_SEARCH_LIMIT = 20;
export const HISTORY_VECTOR_MATCH_THRESHOLD = 0.5;
export const MAX_HISTORY_QUERY_BYTES = 4_096;
export const MAX_HISTORY_TOOL_TEXT_BYTES = 4_096;
export const MAX_HISTORY_TOOL_TITLE_BYTES = 512;
export const MAX_HISTORY_TOOL_SNIPPET_BYTES = 1_024;
const THREAD_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[78][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TURN_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const EXACT_IDENTIFIER = /^(?=.{2,128}$)(?=.*[_:])[\p{L}\p{N}][\p{L}\p{N}._:-]*$/u;
const HISTORY_TERM = /[\p{L}\p{N}_-]+/gu;
const HISTORY_SEARCH_STOP_WORDS = new Set([
  "a", "about", "an", "and", "are", "be", "been", "being", "did", "do", "does",
  "for", "from", "how", "i", "in", "is", "it", "me", "my", "of", "on", "or",
  "our", "that", "the", "this", "to", "was", "we", "were", "what", "when", "where",
  "which", "who", "why", "with", "you", "your",
]);

export type HistoryFindSessionsInput = Readonly<{
  query: string;
  limit: number;
}>;

export type HistoryReadSessionInput = Readonly<{
  session_id: string;
  turn_ids?: readonly string[];
}>;

export type HistorySource = Readonly<{
  turn_id: string;
  cursor: string;
}>;

export type HistoryCitation = Readonly<{
  thread_id: string;
  title: string;
  sources: readonly HistorySource[];
}>;

export type HistorySearchHit = Readonly<{
  thread_id: string;
  title: string;
  turn_id: string;
  cursor: string;
  score: number;
  snippet: string;
}>;

export type HistoryFindSessionsResponse = Readonly<{
  query: string;
  results: readonly HistorySearchHit[];
  citations: readonly HistoryCitation[];
}>;

export type HistoryThreadTurn = Readonly<{
  thread_id: string;
  title: string;
  turn_id: string;
  cursor: string;
  user: string;
  assistant: string;
}>;

export type HistoryReadSessionResponse = Readonly<{
  turns: readonly HistoryThreadTurn[];
  citations: readonly HistoryCitation[];
}>;

export type HistoryProjection = Readonly<{
  thread_id: string;
  turn_id: string;
  cursor: string;
  title: string;
  input: SessionPromptInput;
  final_message: string;
  created_at: number;
}>;

export class HistorySearchError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

export function parseHistoryFindSessionsInput(value: unknown): HistoryFindSessionsInput {
  if (!isRecord(value)) {
    throw new HistorySearchError(400, "invalid_request", "session search body must be a JSON object");
  }
  if (Object.keys(value).some((key) => !["query", "limit"].includes(key))) {
    throw new HistorySearchError(400, "invalid_request", "supported fields are query and limit");
  }
  if (typeof value.query !== "string" || !value.query.trim()) {
    throw new HistorySearchError(400, "invalid_query", "query must be a non-empty string");
  }
  if (new TextEncoder().encode(value.query).byteLength > MAX_HISTORY_QUERY_BYTES) {
    throw new HistorySearchError(
      400,
      "invalid_query",
      `query must not exceed ${MAX_HISTORY_QUERY_BYTES} bytes`,
    );
  }
  if (value.limit !== undefined && (!Number.isSafeInteger(value.limit)
    || Number(value.limit) < 1 || Number(value.limit) > MAX_HISTORY_SEARCH_LIMIT)) {
    throw new HistorySearchError(
      400,
      "invalid_limit",
      `limit must be an integer from 1 to ${MAX_HISTORY_SEARCH_LIMIT}`,
    );
  }
  return {
    query: value.query.trim(),
    limit: value.limit === undefined ? DEFAULT_LIMIT : Number(value.limit),
  };
}

export function parseHistoryReadSessionInput(value: unknown): HistoryReadSessionInput {
  if (!isRecord(value)
    || Object.keys(value).some((key) => !["session_id", "turn_ids"].includes(key))) {
    throw new HistorySearchError(400, "invalid_request", "supported fields are session_id and turn_ids");
  }
  if (typeof value.session_id !== "string" || !THREAD_ID.test(value.session_id)) {
    throw new HistorySearchError(400, "invalid_session_id", "invalid session id");
  }
  if (value.turn_ids !== undefined && (!Array.isArray(value.turn_ids)
    || value.turn_ids.length > MAX_HISTORY_SEARCH_LIMIT
    || value.turn_ids.some((turnId) => typeof turnId !== "string" || !TURN_ID.test(turnId)))) {
    throw new HistorySearchError(
      400,
      "invalid_turn_ids",
      `turn_ids must contain at most ${MAX_HISTORY_SEARCH_LIMIT} valid turn ids`,
    );
  }
  return {
    session_id: value.session_id,
    ...(value.turn_ids === undefined ? {} : { turn_ids: value.turn_ids as string[] }),
  };
}

/** Exact identifiers are better served by FTS than semantic similarity. */
export function isExactHistoryIdentifierQuery(query: string): boolean {
  return EXACT_IDENTIFIER.test(query.normalize("NFKC").trim());
}

export function historyVectorRetrieval(organizationId: string, teamId: string, limit: number) {
  return {
    retrieval_type: "vector" as const,
    // AI Search applies this to vector similarity before result limiting. A
    // query with no sufficiently related memory therefore returns no direct
    // results instead of filling the response with nearest-but-irrelevant
    // turns.
    match_threshold: HISTORY_VECTOR_MATCH_THRESHOLD,
    max_num_results: Math.min(50, Math.max(limit, limit * 3)),
    filters: {
      organization_id: { $eq: organizationId },
      team_id: { $eq: teamId },
    },
    return_on_failure: false,
  };
}

export function historySearchTerms(
  query: string,
  { includeStopWords = false, maxTerms = 24 } = {},
): string[] {
  const terms = query.normalize("NFKC").match(HISTORY_TERM) ?? [];
  return [...new Set(terms.map((term) => term.toLocaleLowerCase()))]
    .filter((term) => includeStopWords || !HISTORY_SEARCH_STOP_WORDS.has(term))
    .slice(0, maxTerms);
}

export function historyFtsQuery(query: string): string {
  const terms = historySearchTerms(query, {
    includeStopWords: isExactHistoryIdentifierQuery(query),
  });
  return terms.map((term) => `"${term.replaceAll('"', '""')}"`).join(" OR ");
}

export function isAcceptedHistoryLexicalMatch(query: string, content: string): boolean {
  if (isExactHistoryIdentifierQuery(query)) {
    const identifier = query.normalize("NFKC").trim()
      .replace(/[.*+?^$\{\}()|[\]\\]/gu, "\\$&");
    return new RegExp(
      `(?:^|[^\\p{L}\\p{N}._:-])${identifier}`
        + "(?=$|[^\\p{L}\\p{N}._:-]|\\.(?=$|\\s))",
      "iu",
    ).test(content.normalize("NFKC"));
  }
  const terms = historySearchTerms(query, {
    includeStopWords: false,
  });
  if (terms.length === 0) return false;
  const contentTerms = new Set(historySearchTerms(content, {
    includeStopWords: true,
    maxTerms: Number.MAX_SAFE_INTEGER,
  }));
  for (const term of [...contentTerms]) {
    for (const part of term.split(/[_-]+/u)) {
      if (part) contentTerms.add(part);
    }
  }
  const matched = terms.reduce((count, term) => count + Number(contentTerms.has(term)), 0);
  if (terms.length === 1) return matched === 1;
  if (terms.length === 2) {
    const words = content.normalize("NFKC").toLocaleLowerCase()
      .replace(/[_-]+/gu, " ")
      .match(/[\p{L}\p{N}]+/gu) ?? [];
    return words.some((word, index) => word === terms[0] && words[index + 1] === terms[1]);
  }
  return matched >= Math.max(2, Math.ceil(terms.length * 0.6));
}

export function groupHistoryCitations(
  results: readonly Pick<HistorySearchHit, "thread_id" | "title" | "turn_id" | "cursor">[],
): HistoryCitation[] {
  const grouped = new Map<string, { title: string; sources: HistorySource[] }>();
  for (const result of results) {
    let citation = grouped.get(result.thread_id);
    if (!citation) {
      citation = { title: result.title, sources: [] };
      grouped.set(result.thread_id, citation);
    }
    if (!citation.sources.some((source) => source.turn_id === result.turn_id
      && source.cursor === result.cursor)) {
      citation.sources.push({ turn_id: result.turn_id, cursor: result.cursor });
    }
  }
  return [...grouped].map(([thread_id, citation]) => ({ thread_id, ...citation }));
}

export function mergeHistoryCitations(
  current: readonly HistoryCitation[],
  added: readonly HistoryCitation[],
): HistoryCitation[] {
  return groupHistoryCitations([...current, ...added].flatMap((citation) => (
    citation.sources.map((source) => ({
      thread_id: citation.thread_id,
      title: citation.title,
      turn_id: source.turn_id,
      cursor: source.cursor,
    }))
  )));
}

export function promptInputText(input: SessionPromptInput): string {
  if (typeof input === "string") return input;
  return input.flatMap((item) => {
    if (item.type === "text") return [item.text];
    if (item.type === "image") return ["[image]"];
    if (item.type === "audio") return ["[audio]"];
    return [];
  }).join("\n");
}

export const FIND_SESSIONS_TOOL_DESCRIPTION = [
  "Find bounded candidate completed sessions in the active team's Nanocodex history.",
  "Use read_session to verify relevant candidates before answering.",
].join(" ");

export const READ_SESSION_TOOL_DESCRIPTION = [
  "Read exact completed turns from one candidate Nanocodex session in the active team.",
  "Pass turn_ids to select exact search hits, or omit them to read the newest bounded context.",
].join(" ");

export function findSessionsToolInputSchema() {
  return {
    type: "object",
    properties: {
      query: { type: "string", minLength: 1, maxLength: MAX_HISTORY_QUERY_BYTES },
      limit: {
        type: "integer",
        minimum: 1,
        maximum: MAX_HISTORY_SEARCH_LIMIT,
        default: DEFAULT_LIMIT,
      },
    },
    required: ["query"],
    additionalProperties: false,
  } as const;
}

export function readSessionToolInputSchema() {
  return {
    type: "object",
    properties: {
      session_id: { type: "string", pattern: THREAD_ID.source },
      turn_ids: {
        type: "array",
        items: { type: "string", pattern: TURN_ID.source },
        maxItems: MAX_HISTORY_SEARCH_LIMIT,
      },
    },
    required: ["session_id"],
    additionalProperties: false,
  } as const;
}

export type FindSessionsToolResult = Readonly<{
  sessions: readonly Readonly<{
    session_id: string;
    title: string;
    turn_id: string;
    cursor: string;
    score: number;
    preview: string;
  }>[];
}>;

export type ReadSessionToolResult = Readonly<{
  turns: readonly Readonly<{
    session_id: string;
    title: string;
    turn_id: string;
    cursor: string;
    user: string;
    assistant: string;
  }>[];
}>;

/**
 * Projects the internal search response onto the model-visible contract. The
 * allowlist and UTF-8 truncation prevent storage/provider metadata or a single
 * oversized transcript from leaking into a tool result.
 */
export function projectFindSessionsToolResult(
  value: unknown,
  limit: number,
): FindSessionsToolResult {
  if (!isRecord(value) || !Array.isArray(value.results)
    || value.results.length > MAX_HISTORY_SEARCH_LIMIT
    || !Number.isSafeInteger(limit) || limit < 1 || limit > MAX_HISTORY_SEARCH_LIMIT) {
    throw new HistorySearchError(502, "invalid_response", "session search response is malformed");
  }
  return {
    sessions: value.results.slice(0, limit).map((candidate) => {
      if (!isRecord(candidate)
        || typeof candidate.thread_id !== "string" || !THREAD_ID.test(candidate.thread_id)
        || typeof candidate.turn_id !== "string" || !TURN_ID.test(candidate.turn_id)
        || typeof candidate.cursor !== "string" || !/^\d+$/.test(candidate.cursor)
        || typeof candidate.title !== "string"
        || typeof candidate.snippet !== "string"
        || typeof candidate.score !== "number" || !Number.isFinite(candidate.score)
        || candidate.score < 0 || candidate.score > 1) {
        throw new HistorySearchError(502, "invalid_response", "session search result is malformed");
      }
      return {
        session_id: candidate.thread_id,
        title: truncateUtf8(candidate.title, MAX_HISTORY_TOOL_TITLE_BYTES),
        turn_id: candidate.turn_id,
        cursor: candidate.cursor,
        score: candidate.score,
        preview: truncateUtf8(candidate.snippet, MAX_HISTORY_TOOL_SNIPPET_BYTES),
      };
    }),
  };
}

/** Projects only bounded transcript fields; citations remain Worker-owned. */
export function projectReadSessionToolResult(value: unknown): ReadSessionToolResult {
  if (!isRecord(value) || !Array.isArray(value.turns)
    || value.turns.length > MAX_HISTORY_SEARCH_LIMIT) {
    throw new HistorySearchError(502, "invalid_response", "session read response is malformed");
  }
  return {
    turns: value.turns.map((candidate) => {
      if (!isRecord(candidate)
        || typeof candidate.thread_id !== "string" || !THREAD_ID.test(candidate.thread_id)
        || typeof candidate.turn_id !== "string" || !TURN_ID.test(candidate.turn_id)
        || typeof candidate.cursor !== "string" || !/^\d+$/.test(candidate.cursor)
        || typeof candidate.title !== "string"
        || typeof candidate.user !== "string"
        || typeof candidate.assistant !== "string") {
        throw new HistorySearchError(502, "invalid_response", "session turn is malformed");
      }
      return {
        session_id: candidate.thread_id,
        title: truncateUtf8(candidate.title, MAX_HISTORY_TOOL_TITLE_BYTES),
        turn_id: candidate.turn_id,
        cursor: candidate.cursor,
        user: truncateUtf8(candidate.user, MAX_HISTORY_TOOL_TEXT_BYTES),
        assistant: truncateUtf8(candidate.assistant, MAX_HISTORY_TOOL_TEXT_BYTES),
      };
    }),
  };
}

function truncateUtf8(value: string, maximum: number): string {
  const encoder = new TextEncoder();
  if (encoder.encode(value).byteLength <= maximum) return value;
  let result = "";
  let bytes = 0;
  for (const character of value) {
    const width = encoder.encode(character).byteLength;
    if (bytes + width > maximum) break;
    result += character;
    bytes += width;
  }
  return result;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
