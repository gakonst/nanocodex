// Long, rich, live conversation driven through the real controller and terminal view.
// A synthetic Agent replays history and streams a live turn one event per frame.
import React, { Profiler, useState } from "react";
import { createRoot } from "react-dom/client";
import type { ToolActivity } from "nanocodex-react/agent";
import { AgentTerminalView } from "../../../nanocodex-terminal/src/AgentTerminalView";
import "../../../nanocodex-terminal/styles.css";

type Event = { request_id: string; seq: number; type: string; payload: Record<string, unknown> };
const session = "chat-performance";
let seq = 0;
const ev = (type: string, payload: Record<string, unknown>): Event => ({ request_id: session, seq: ++seq, type, payload });
const t0 = Date.now() - 3_600_000;

function answer(turn: number): string {
  const rows = Array.from({ length: 5 }, (_, i) => `| svc-${turn}-${i} | ${i % 2 ? "ready" : "degraded"} | ${100 + turn * 3 + i} ms | owner ${i} |`).join("\n");
  const code = Array.from({ length: 18 }, (_, i) => `  const step${i} = await checks.run("turn-${turn}-${i}", { retries: ${i % 4} });`).join("\n");
  return `## Turn ${turn} summary\n\nI reviewed **${turn + 3} services** and patched \`release.ts\`. See [the runbook](https://example.com/runbook/${turn}).\n\n`
    + `| Service | State | Latency | Owner |\n| --- | --- | --- | --- |\n${rows}\n\n`
    + `1. Read the release module\n2. Patched the readiness gate\n   - verified rollback\n   - recorded metrics\n3. Ran the suite\n\n`
    + "```typescript\nexport async function verify() {\n" + code + "\n  return true;\n}\n```\n\n"
    + (turn === 4 ? "```mermaid\nflowchart LR\n  A[Read] --> B[Patch]\n  B --> C[Test]\n```\n\n" : "")
    + `> Turn ${turn}: rollout is gated on the readiness check.\n`;
}

const out = (lines: number, label: string) => Array.from({ length: lines }, (_, i) => `${label} line ${i + 1}: ok`).join("\n");
const patch = (turn: number) => `*** Begin Patch\n*** Update File: src/release-${turn}.ts\n@@ export function ship\n-  const ready = false;\n+  const ready = await checks.pass(${turn});\n+  if (!ready) throw new Error("blocked");\n   return deploy(region);\n*** End Patch`;

function tool(turn: string, id: string, name: string, args: unknown, result: unknown, at: number, extra: Record<string, unknown> = {}): Event[] {
  return [
    ev("tool.call", { turn_id: turn, call_id: id, tool: name, arguments: args, managed_event_created_at: at, ...extra }),
    ev("tool.result", { turn_id: turn, call_id: id, tool: name, status: "completed", result, duration_ns: 1_200_000_000, managed_event_created_at: at + 1200, ...extra }),
  ];
}

export function historyEvents(turns = 30): Event[] {
  const events: Event[] = [];
  for (let t = 0; t < turns; t++) {
    const turn = `turn-${t}`, at = t0 + t * 60_000;
    events.push(ev("managed.prompt", { turn_id: turn, text: `Turn ${t}: fix the release gate for region ${t} and verify it.` }));
    events.push(ev("run.started", { turn_id: turn }));
    events.push(ev("reasoning.summary.delta", { turn_id: turn, item_id: `r-${t}`, text: `**Planning** Read release-${t}.ts, patch the gate, run tests.\nThen summarize.` }));
    events.push(...tool(turn, `cmd-${t}`, "exec_command", { cmd: `rg -n ready src/release-${t}.ts` }, { exit_code: 0, output: out(30, "rg") }, at));
    events.push(...tool(turn, `read-${t}`, "Read", { file_path: `src/release-${t}.ts`, offset: 1, limit: 40 }, out(40, "src"), at + 2000));
    events.push(...tool(turn, `patch-${t}`, "apply_patch", patch(t), "Done", at + 4000));
    if (t % 3 === 0) {
      events.push(ev("tool.call", { turn_id: turn, call_id: `code-${t}`, tool: "exec", arguments: { code: "await tools.exec_command({cmd:'pnpm build'})" }, managed_event_created_at: at + 5000 }));
      events.push(...tool(turn, `code-${t}/code-1`, "exec_command", { cmd: "pnpm build" }, { exit_code: 0, output: out(8, "build") }, at + 5100));
      events.push(ev("tool.result", { turn_id: turn, call_id: `code-${t}`, tool: "exec", status: "completed", result: "built", duration_ns: 2e9 }));
    }
    if (t % 4 === 1) {
      const agent = 100 + t;
      events.push(...tool(turn, `spawn-${t}`, "spawn_agent", { role: "auditor", task: `Audit turn ${t}` }, { agent_id: agent, role: "auditor" }, at + 6000));
      events.push(...tool(turn, `child-${t}`, "Read", { file_path: `src/release-${t}.ts` }, out(10, "child"), at + 6500, { managed_agent_id: agent }));
      events.push(ev("assistant.message", { turn_id: turn, managed_agent_id: agent, text: `Audit ${t}: the readiness gate is safe.\n\n- no regressions\n- rollback verified` }));
    }
    if (t % 5 === 2) events.push(...tool(turn, `preview-${t}`, "preview", { port: 3000 }, { url: `https://preview-${t}.example.com/release`, port: 3000 }, at + 7000));
    events.push(ev("assistant.message", { turn_id: turn, text: answer(t) }));
    events.push(ev("run.completed", { turn_id: turn }));
  }
  return events;
}

/** One live turn: thinking, tools, a streaming subagent, then a long streamed answer. */
export function liveEvents(): Event[] {
  const turn = "turn-live", now = Date.now(), agent = 900;
  const events: Event[] = [ev("managed.prompt", { turn_id: turn, text: "Run the whole release check again and summarize." }), ev("run.started", { turn_id: turn })];
  const thinking = "Re-running the suite. Checking the flaky release test and the rollout gate across regions. ".split(" ");
  thinking.forEach(word => events.push(ev("reasoning.summary.delta", { turn_id: turn, item_id: "r-live", text: `${word} ` })));
  events.push(ev("tool.call", { turn_id: turn, call_id: "live-cmd", tool: "exec_command", arguments: { cmd: "pnpm test --watch=false" }, managed_event_created_at: now }));
  events.push(ev("tool.result", { turn_id: turn, call_id: "live-cmd", tool: "exec_command", status: "completed", result: { exit_code: 0, output: out(30, "test") }, managed_event_created_at: now + 3000 }));
  events.push(...tool(turn, "live-spawn", "spawn_agent", { role: "reviewer", task: "Review the rerun" }, { agent_id: agent, role: "reviewer" }, now + 3100));
  events.push(ev("tool.call", { turn_id: turn, call_id: "live-child", tool: "Read", arguments: { file_path: "src/release.ts" }, managed_agent_id: agent }));
  events.push(ev("tool.result", { turn_id: turn, call_id: "live-child", tool: "Read", status: "completed", result: out(12, "child"), managed_agent_id: agent }));
  "The rerun is clean and the gate blocks unsafe rollouts. No regressions found in any region. ".split(" ")
    .forEach(word => events.push(ev("assistant.delta", { turn_id: turn, managed_agent_id: agent, item_id: "child-a", text: `${word} ` })));
  const final = answer(99).replace("## Turn 99 summary", "## Rerun summary");
  for (let i = 0; i < final.length; i += 12) events.push(ev("assistant.delta", { turn_id: turn, item_id: "a-live", text: final.slice(i, i + 12) }));
  events.push(ev("assistant.message", { turn_id: turn, item_id: "a-live", text: final }));
  events.push(ev("run.completed", { turn_id: turn }));
  return events;
}

let onEvent: (event: Event) => void = () => {};
let onHistory: (events: readonly Event[]) => void = () => {};
const agent = {
  sessionId: session,
  events: { watch: () => ({
    onEvent(listener: typeof onEvent) { onEvent = listener; return () => { onEvent = () => {}; }; },
    onHistory(listener: typeof onHistory) { onHistory = listener; return () => { onHistory = () => {}; }; },
    off() {},
  }) },
  turn: { prompt: () => ({ steer: async () => {}, cancel: async () => {}, result: () => new Promise(() => {}), dispose() {} }) },
};

const commits: Array<{ phase: string; actual: number; base: number }> = [];
const perf = { phase: "idle", commits, historyEvents, liveEvents,
  history(turns?: number) { onHistory(historyEvents(turns)); },
  emit(event: Event) { onEvent(event); } };
(window as any).perf = perf;

// Mirrors the account: a fresh inline renderTool on every parent render.
function AccessoryCard({ tool }: { tool: ToolActivity; onReceipt(receipt: string): void }) {
  return tool.name === "request_vault_intake" ? <div>Vault intake</div> : null;
}
function Harness() {
  const [, setState] = useState(0);
  (window as any).rerenderHost = () => setState(n => n + 1);
  return <Profiler id="chat" onRender={(_id, _phase, actual, base) => commits.push({ phase: perf.phase, actual, base })}>
    <AgentTerminalView agent={agent as any} agentError={undefined} mode="full" onConversationActivity={() => {}}
      onStateChange={() => {}} retryAgent={() => {}} promptIntent="queue"
      renderTool={(tool, { submit }) => <AccessoryCard key={tool.callId} tool={tool} onReceipt={submit} />} />
  </Profiler>;
}
createRoot(document.getElementById("root")!).render(<Harness />);
