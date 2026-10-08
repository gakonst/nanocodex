// Synthetic transcript driven by the agent-activity journey through window.fixture.
import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { TerminalTranscriptSurface } from "../../../nanocodex-terminal/src/TerminalTranscriptSurface";
import "../../../nanocodex-terminal/styles.css";

const t0 = Date.now() - 120_000;
const json = (value: unknown) => JSON.stringify(value);
const tool = (callId: string, name: string, status: string, input: unknown, output?: unknown, extra: Record<string, unknown> = {}) => ({
  callId, name, status, arguments: "", children: [], startedAtMs: t0, durationNs: status === "running" ? undefined : 2_400_000_000,
  input: typeof input === "string" ? input : json(input),
  ...(output === undefined ? {} : { output: typeof output === "string" ? output : json(output) }),
  ...extra,
});
const image = "data:image/svg+xml;base64," + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="480" height="180"><rect width="480" height="180" rx="14" fill="#dfe8f2"/><text x="28" y="100" font-family="sans-serif" font-size="26">Dashboard screenshot</text></svg>');
const patch = "*** Begin Patch\n*** Update File: src/release.ts\n@@ export function ship\n-  const ready = false;\n+  const ready = await checks.pass();\n+  if (!ready) throw new Error(\"blocked\");\n   return deploy(region);\n*** Add File: src/notes.md\n+# Release notes\n+Rolled out gradually.\n*** End Patch";

const filler = Array.from({ length: 14 }, (_, index) => ({ id: `history-${index}`, kind: "assistant", streaming: false,
  text: `Earlier note ${index + 1}: the review covered configuration, rollout timing, and monitoring for region ${index + 1}.` }));

export const completed = [
  ...filler,
  { id: "u1", kind: "user", text: "Fix the release check, show me the dashboard, and ask an auditor to review." },
  { id: "r1", kind: "reasoning", streaming: false, text: "**Planning** I should read the release module, patch the check, and run the tests." },
  { id: "t-read", kind: "tool", tool: tool("read", "Read", "completed", { file_path: "src/release.ts", offset: 1, limit: 40 }, "export async function ship(region) {\n  const ready = false;\n  return deploy(region);\n}") },
  { id: "t-search", kind: "tool", tool: tool("grep", "exec_command", "completed", { cmd: "rg -n 'ready' src", workdir: "/workspace" }, "Process exited with code 0\nOutput:\nsrc/release.ts:2:  const ready = false;") },
  { id: "t-edit", kind: "tool", tool: tool("edit", "Edit", "completed", { file_path: "src/config.ts", old_string: "timeout: 30", new_string: "timeout: 60,\nretries: 3" }, "ok") },
  { id: "t-patch", kind: "tool", tool: tool("patch", "apply_patch", "completed", patch, "Done") },
  { id: "t-fail", kind: "tool", tool: tool("fail", "exec_command", "failed", { cmd: "pnpm test" }, "Process exited with code 1\nOutput:\nFAIL release.test.ts\nError: expected ready to be true") },
  { id: "t-web", kind: "tool", tool: tool("web", "web__run", "completed", { search_query: [{ q: "gradual rollout checklist" }] }, { results: [1, 2, 3] }) },
  { id: "t-code", kind: "tool", tool: { ...tool("code", "exec", "completed", { code: "const r = await tools.exec_command({cmd:'pnpm build'});\nawait tools.view_image({path:'dash.png'});" }, "built"), children: [
    tool("/code-1", "exec_command", "completed", { cmd: "pnpm build" }, "Process exited with code 0\nOutput:\nbuilt in 2.1s"),
    tool("/code-2", "view_image", "completed", { path: "dash.png" }, "image", { images: [image] }),
  ] } },
  { id: "t-browser", kind: "tool", tool: tool("browser", "browser_execute", "completed", { code: "await page.goto('https://dashboard.example.com/releases')" }, "ok") },
  { id: "t-preview", kind: "tool", tool: tool("preview", "preview", "completed", { port: 3000 }, { url: "https://preview.example.com/release", port: 3000 }) },
  { id: "t-spawn", kind: "tool", tool: tool("spawn", "spawn_agent", "completed", { role: "auditor", task: "Review the release patch" }, { agent_id: 3, role: "auditor" }) },
  { id: "c1", kind: "tool", turnId: "turn-1", responseIdentity: { agentId: 3 }, tool: tool("child-read", "Read", "completed", { file_path: "src/release.ts" }, "export async function ship() {}") },
  { id: "c2", kind: "assistant", turnId: "turn-1", responseIdentity: { agentId: 3 }, streaming: false, text: "The patch is safe: the readiness check now blocks unsafe rollouts." },
  { id: "t-mcp", kind: "tool", tool: tool("mcp", "mcp__linear__create_issue", "completed", { title: "Follow up" }, { id: "LIN-1" }) },
  { id: "t-unknown", kind: "tool", tool: tool("unknown", "mystery_tool", "completed", { anything: true }, { ok: true }) },
  { id: "a1", kind: "assistant", streaming: false, text: "Fixed the readiness check. One test still fails; see the failed command above." },
];

function Harness() {
  const [state, setState] = useState({ entries: completed as any[], running: false, activity: "Ready" });
  (window as any).fixture = {
    set: (patch: Partial<typeof state>) => setState(previous => ({ ...previous, ...patch })),
    append: (...entries: any[]) => setState(previous => ({ ...previous, entries: [...previous.entries, ...entries] })),
    replace: (id: string, entry: any) => setState(previous => ({ ...previous, entries: previous.entries.map(item => item.id === id ? entry : item) })),
    tool, completed,
  };
  return <TerminalTranscriptSurface entries={state.entries} running={state.running} activity={state.activity}
    composer={<div className="fixture-composer">Ask anything</div>} canLoadOlder={false} isLoadingOlder={false}
    inactiveMessage="" mode="full" status="ready" onLoadOlder={async () => false} />;
}
createRoot(document.getElementById("root")!).render(<Harness />);
