import { classifyReplyRequest, GMAIL_DECISION_POLICY } from "./gmail-firehose-decisions";
import type { RoutingAi } from "./thread-model-routing";
import type { Principal } from "./account-auth";

/** Explicit gateway ID makes Jev calls inspectable in AI Gateway logs. No key is passed here. */
export function jevGatewayBinding(ai: RoutingAi, id = "default"): RoutingAi {
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(id)) throw new Error("invalid_jev_gateway_id");
  return {run: (model, input) => (ai as RoutingAi & {run(model: string, input: unknown,
    options: {gateway:{id:string;collectLog:boolean;skipCache:boolean}}):Promise<unknown>}).run(model,input,{gateway:{id,collectLog:false,skipCache:true}})};
}
const thresholds = [0.65, 0.75, 0.85, 0.9, 0.95] as const;
const reply = (data: unknown, status = 200) => Response.json(data,{status,headers:{"cache-control":"no-store"}});
type Sample = {id:string;expected:"reply"|"no_reply";from:string;subject:string;body:string};
function validSample(value: unknown): value is Sample {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const sample = value as Record<string,unknown>;
  return Object.keys(sample).sort().join(",") === "body,expected,from,id,subject"
    && typeof sample.id === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(sample.id)
    && (sample.expected === "reply" || sample.expected === "no_reply")
    && typeof sample.from === "string" && sample.from.length > 0 && sample.from.length <= 256
    && typeof sample.subject === "string" && sample.subject.length <= 256
    && typeof sample.body === "string" && sample.body.trim().length > 0
    && new TextEncoder().encode(sample.body).length <= 8000;
}
async function boundedJson(request: Request): Promise<unknown> {
  if (!request.body || request.headers.get("content-type")?.split(";")[0] !== "application/json") return null;
  const reader = request.body.getReader(); const chunks: Uint8Array[] = []; let length = 0;
  try {
    for (;;) {
      const {done,value} = await reader.read(); if (done) break;
      length += value.byteLength; if (length > 32768) return null;
      chunks.push(value);
    }
  } finally {await reader.cancel().catch(()=>{});reader.releaseLock();}
  const bytes = new Uint8Array(length);let offset=0;
  for (const chunk of chunks) {bytes.set(chunk,offset);offset+=chunk.byteLength;}
  try {return JSON.parse(new TextDecoder("utf-8",{fatal:true,ignoreBOM:false}).decode(bytes));} catch {return null;}
}
/** Private, non-persistent labeled fixture runner; no actual Gmail content is fetched. */
export async function routeGmailDecisionBacktest(request: Request, ai: RoutingAi | undefined,
  principal: Principal | null | undefined, enabledOwnerId: string | undefined): Promise<Response> {
  const url = new URL(request.url);
  if (!principal) return reply({error:"unauthorized"},401);
  if (principal.connectGrant || !["api_key","account_session"].includes(principal.kind)
    || !principal.capabilities.includes("agents:write") || !enabledOwnerId || principal.userId !== enabledOwnerId)
    return reply({error:"forbidden"},403);
  if (principal.kind === "account_session" && request.headers.get("origin") !== url.origin)
    return reply({error:"forbidden_origin"},403);
  if (request.method !== "POST" || url.search) return reply({error:"invalid_request"},400);
  if (!ai) return reply({error:"jev_unavailable"},503);
  const raw = await boundedJson(request);
  if (!raw || typeof raw !== "object" || Array.isArray(raw) || Object.keys(raw).join(",") !== "samples")
    return reply({error:"invalid_fixtures"},400);
  const samples = (raw as {samples?:unknown}).samples;
  if (!Array.isArray(samples) || !samples.length || samples.length > 5 || !samples.every(validSample)
    || new Set(samples.map(sample => sample.id)).size !== samples.length)
    return reply({error:"invalid_fixtures"},400);
  const rows = [];
  for (const sample of samples as Sample[]) {
    const result = await classifyReplyRequest(ai,{id:sample.id,status:"ok",
      headers:{from:sample.from,subject:sample.subject},body:sample.body});
    rows.push({id:sample.id,expected:sample.expected,choice:result.choice,outcome:result.outcome,
      reason:result.reason,classifier_outcome:result.classifier_outcome,confidence:result.confidence,
      reply_probability:result.reply_probability,duration_ms:result.duration_ms});
  }
  const byThreshold: Record<string,Record<string,number>> = {};
  for (const threshold of thresholds) {
    const metrics = {true_positive:0,true_negative:0,false_positive:0,false_negative:0,abstained:0};
    for (const row of rows) {
      if (row.choice === null || row.confidence === null || row.confidence < threshold) {metrics.abstained++;continue;}
      const predicted = row.choice === "reply_requested" ? "reply" : "no_reply";
      const key = predicted === "reply" ? row.expected === "reply" ? "true_positive" : "false_positive"
        : row.expected === "reply" ? "false_negative" : "true_negative";
      metrics[key]++;
    }
    byThreshold[String(threshold)] = metrics;
  }
  return reply({policy_version:GMAIL_DECISION_POLICY,model:"typesafe/jev",rows,thresholds:byThreshold,
    note:"User-supplied labels are independent ground truth; Jev confidence is not calibrated accuracy. No fixtures are stored."});
}
