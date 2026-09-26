/** Account-private, bounded decision metadata. Only bounded display headers; never persist bodies or model prompts. */
export const GMAIL_TRACE_POLICY = "gmail-reply-triage-v1";
export const GMAIL_TRACE_REASONS = ["explicit_reply", "no_reply", "low_confidence", "invalid_result",
  "timeout", "rate_limited", "unavailable", "binding_error", "missing_body", "truncated", "missing_headers"] as const;
export type GmailTraceReason = typeof GMAIL_TRACE_REASONS[number];
export type GmailDecisionTrace = Readonly<{
  sender?: string; subject?: string; source_url?: string;
  source_key: string; policy_version: typeof GMAIL_TRACE_POLICY;
  outcome: "reply" | "no_reply" | "unavailable" | "filtered";
  reason: GmailTraceReason;
  classifier_outcome: "success" | "timeout" | "rate_limited" | "unavailable" | "binding_error" | "invalid_result" | "not_requested";
  confidence: number | null; reply_probability: number | null;
  duration_ms: number; decision_id: string | null;
}>;
const sourceKey = /^gmail:gmail-reply-triage-v1:[a-f0-9]{64}$/;
const decisionId = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const outcomes = new Set(["reply", "no_reply", "unavailable", "filtered"]);
const classifierOutcomes = new Set(["success", "timeout", "rate_limited", "unavailable", "binding_error", "invalid_result", "not_requested"]);
const boundedProbability = (value: unknown) => value === null || typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1;
export function validGmailDecisionTrace(input: GmailDecisionTrace): boolean {
  if (!input || typeof input !== "object" || Array.isArray(input)
    || Object.keys(input).filter(key => !["sender", "subject", "source_url"].includes(key)).sort().join(",") !== ["classifier_outcome", "confidence", "decision_id", "duration_ms", "outcome", "policy_version", "reason", "reply_probability", "source_key"].join(",")
    || !sourceKey.test(input.source_key) || input.policy_version !== GMAIL_TRACE_POLICY
    || !outcomes.has(input.outcome) || !GMAIL_TRACE_REASONS.includes(input.reason)
    || !classifierOutcomes.has(input.classifier_outcome)
    || !boundedProbability(input.confidence) || !boundedProbability(input.reply_probability)
    || !Number.isSafeInteger(input.duration_ms) || input.duration_ms < 0 || input.duration_ms > 120_000
    || (input.decision_id !== null && (typeof input.decision_id !== "string" || !decisionId.test(input.decision_id)))) return false;
  for (const key of ["sender", "subject"] as const) {
    const value = input[key];
    if (value !== undefined && (typeof value !== "string" || new TextEncoder().encode(value).length > 256
      || /[\u0000-\u001f\u007f]/.test(value))) return false;
  }
  if (input.source_url !== undefined && input.source_url !== "" && (typeof input.source_url !== "string" || !/^https:\/\/mail\.google\.com\/mail\/u\/0\/#all\/[A-Za-z0-9_-]{1,128}$/.test(input.source_url))) return false;
  if ((input.outcome === "reply") !== (input.decision_id !== null)) return false;
  return true;
}
export function initializeGmailDecisionTraces(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS gmail_decision_traces (
    id INTEGER PRIMARY KEY AUTOINCREMENT, source_key TEXT NOT NULL UNIQUE,
    policy_version TEXT NOT NULL, outcome TEXT NOT NULL, reason TEXT NOT NULL,
    classifier_outcome TEXT NOT NULL, confidence REAL, reply_probability REAL,
    duration_ms INTEGER NOT NULL, decision_id TEXT,
    first_at INTEGER NOT NULL, observed_at INTEGER NOT NULL, seen_count INTEGER NOT NULL DEFAULT 1
  )`);
  // Existing account databases predate display headers. Keep old rows readable.
  const columns = new Set(storage.sql.exec<{name:string}>("PRAGMA table_info(gmail_decision_traces)").toArray().map(row => row.name));
  for (const column of ["sender", "subject", "source_url"]) {
    if (!columns.has(column)) storage.sql.exec(`ALTER TABLE gmail_decision_traces ADD COLUMN ${column} TEXT NOT NULL DEFAULT ''`);
  }
}
export function recordGmailDecisionTrace(storage: DurableObjectStorage, value: GmailDecisionTrace): void {
  if (!validGmailDecisionTrace(value)) throw new Error("invalid_gmail_decision_trace");
  const now = Date.now();
  storage.sql.exec(`INSERT INTO gmail_decision_traces
    (source_key,policy_version,outcome,reason,classifier_outcome,confidence,reply_probability,duration_ms,decision_id,first_at,observed_at,sender,subject,source_url)
    VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(source_key) DO UPDATE SET
      sender=COALESCE(NULLIF(excluded.sender,''),gmail_decision_traces.sender),
      subject=COALESCE(NULLIF(excluded.subject,''),gmail_decision_traces.subject),
      source_url=COALESCE(NULLIF(excluded.source_url,''),gmail_decision_traces.source_url),
      outcome=excluded.outcome,reason=excluded.reason,classifier_outcome=excluded.classifier_outcome,
      confidence=excluded.confidence,reply_probability=excluded.reply_probability,
      duration_ms=excluded.duration_ms,decision_id=excluded.decision_id,
      observed_at=excluded.observed_at,seen_count=seen_count+1
      WHERE gmail_decision_traces.decision_id IS NULL OR excluded.decision_id IS NOT NULL`,
    value.source_key,value.policy_version,value.outcome,value.reason,value.classifier_outcome,
    value.confidence,value.reply_probability,value.duration_ms,value.decision_id,now,now,value.sender ?? "",value.subject ?? "",value.source_url ?? "");
  storage.sql.exec(`DELETE FROM gmail_decision_traces WHERE observed_at < ? OR
    id NOT IN (SELECT id FROM gmail_decision_traces ORDER BY id DESC LIMIT 5000)`, now - 90 * 86_400_000);
}
export function readGmailDecisionTraces(storage: DurableObjectStorage, query: URLSearchParams): Response {
  const keys = [...query.keys()];
  if (keys.some(key => !["before", "limit"].includes(key)) || new Set(keys).size !== keys.length)
    return Response.json({error:"invalid_request"},{status:400});
  const limitText = query.get("limit") ?? "50", beforeText = query.get("before");
  if (!/^[1-9][0-9]{0,2}$/.test(limitText) || Number(limitText) > 200
    || beforeText !== null && (!/^[1-9][0-9]{0,15}$/.test(beforeText) || !Number.isSafeInteger(Number(beforeText))))
    return Response.json({error:"invalid_request"},{status:400});
  const limit = Number(limitText), before = beforeText === null ? Number.MAX_SAFE_INTEGER : Number(beforeText);
  const rows = storage.sql.exec<{id:number}>(`SELECT id,source_key,policy_version,outcome,reason,classifier_outcome,
    confidence,reply_probability,duration_ms,decision_id,first_at,observed_at,seen_count,sender,subject,source_url
    FROM gmail_decision_traces WHERE id < ? ORDER BY id DESC LIMIT ?`, before, limit + 1).toArray();
  const traces = rows.slice(0, limit);
  return Response.json({traces,next_cursor:rows.length > limit ? traces.at(-1)!.id : null},
    {headers:{"cache-control":"no-store"}});
}

/** A bounded recent feed, independent of the paginated diagnostic endpoint. */
export function recentGmailTodoTraces(storage: DurableObjectStorage) {
  return storage.sql.exec(`SELECT id,sender,subject,source_url,outcome,reason,classifier_outcome,
    confidence,reply_probability,duration_ms,first_at,observed_at,seen_count
    FROM gmail_decision_traces AS trace
    WHERE outcome != 'reply' AND decision_id IS NULL AND observed_at >= ?
      AND NOT EXISTS (SELECT 1 FROM todo_decisions AS decision WHERE decision.source_key = trace.source_key)
    ORDER BY observed_at DESC, id DESC LIMIT 100`, Date.now() - 90 * 86_400_000).toArray();
}
