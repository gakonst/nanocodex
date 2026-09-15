// Bounded live smoke benchmark: six short Luna generations, no tools or stored responses.
// OPENAI_API_KEY is read only from the process environment. Never written to the report.
import WebSocket from "ws";
import { multiplex } from "../runtime/response-lanes.mjs";
import { responseControlsSocket } from "../runtime/response-controls.mjs";
if (!process.env.OPENAI_API_KEY) throw new Error("OPENAI_API_KEY is required");
const started = performance.now();
const socket = new WebSocket("wss://api.openai.com/v1/responses", { headers: { Authorization: `Bearer ${process.env.OPENAI_API_KEY}` } });
const records = [];
const deadline = setTimeout(() => socket.close(), 120_000);
await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
const connectMs = performance.now() - started;
const pool = multiplex(socket);
function run(id, input, expected, previous, controls) {
  const lane = pool.lane(id); const writer = controls ? responseControlsSocket(lane, controls) : lane;
  const start = performance.now(); let firstText; let began;
  const inProgress = new Promise(resolve => { began = resolve; });
  const result = new Promise((resolve, reject) => {
    const cleanup = () => { lane.removeEventListener("message", onMessage); lane.removeEventListener("close", onClose); };
    const onClose = () => { cleanup(); began(); reject(new Error("socket closed before completion")); };
    const onMessage = event => {
      const body = JSON.parse(event.data);
      if (body.type === "response.in_progress") began();
      if (body.type === "response.output_text.delta") firstText ??= performance.now() - start;
      if (["error", "response.failed", "response.incomplete"].includes(body.type)) { cleanup(); began(); reject(new Error(`lane ${id}: ${body.error?.code ?? body.type}`)); }
      if (body.type === "response.completed") {
        cleanup(); began();
        const text = body.response.output.flatMap(item => item.content ?? []).filter(part => part.type === "output_text").map(part => part.text).join("").trim();
        records.push({ lane: id, first_text_ms: firstText, completion_ms: performance.now() - start, correct: text === expected, usage: body.response.usage });
        if (text !== expected) reject(new Error(`lane ${id}: unexpected arithmetic output`));
        else resolve(body.response.id);
      }
    };
    lane.addEventListener("message", onMessage); lane.addEventListener("close", onClose);
    writer.send(JSON.stringify({ type: "response.create", model: "gpt-5.6-luna", reasoning: { effort: "low" }, max_output_tokens: 128, store: false,
      input, ...(previous ? { previous_response_id: previous } : {}) }));
  });
  return { inProgress, result };
}
try {
  const a = run("main", "Return only 17+25 as digits.", "42");
  const b = run("other", "Return only 20+21 as digits.", "41");
  const [parent] = await Promise.all([a.result, b.result]);
  const fork = run("fork", "Add 1 to the previous answer. Digits only.", "43", parent);
  void fork.result.catch(() => {});
  await fork.inProgress;
  const continuation = run("main", "Add 2 to the previous answer. Digits only.", "44", parent);
  await Promise.all([fork.result, continuation.result]);
  const input = [{ role: "developer", content: [{ type: "input_text", text: "Synthetic cache fixture. " + "Stable facts for prefix reuse. ".repeat(500) }] }, { role: "user", content: [{ type: "input_text", text: "Return only 42 as digits." }] }];
  await run("cache", input, "42", undefined, { promptCache: "explicit" }).result;
  await run("cache", input, "42", undefined, { promptCache: "explicit" }).result;
} finally {
  clearTimeout(deadline); pool.close();
  console.log(JSON.stringify({ timestamp: new Date().toISOString(), model: "gpt-5.6-luna", connect_ms: connectMs, complete: records.length === 6, records }, null, 2));
}
