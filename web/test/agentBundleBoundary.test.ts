import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

const application = source("../src/NanocodexApp.tsx");
const entry = source("../src/main.tsx");
const routeLoaders = source("../src/routeLoaders.ts");
const experience = source("../src/AgentExperience.tsx");
const accountSession = source("../src/AccountSession.tsx");
const managedRuntime = source("../src/managedAgentRuntime.ts");
const evals = source("../src/Evals.tsx");
const terminal = source("../src/AgentTerminal.tsx");
const terminalCss = [
  source("../src/AgentTerminal.css"),
  source("../../js/terminal/styles.css"),
].join("\n");
const homeCss = source("../src/Home.css");
const applicationGraph = readdirSync(new URL("../src/", import.meta.url), { recursive: true })
  .filter((path) => /\.(?:js|jsx|ts|tsx)$/.test(String(path)))
  .map((path) => source(`../src/${String(path)}`))
  .join("\n");

test("Vite owns one static application graph without manual module loaders", () => {
  assert.match(application, /import \{ AgentExperience \} from "\.\/AgentExperience"/);
  assert.match(
    experience,
    /import \{ AgentTerminal, ManagedAgentTerminal \} from "\.\/AgentTerminal"/,
  );
  assert.match(
    experience,
    /landing[\s\S]*?<AgentTerminal[\s\S]*?: hasCredential && managedConversationId[\s\S]*?<ManagedAgentTerminal/,
  );
  assert.doesNotMatch(
    applicationGraph,
    /\b(?:lazy|loadAgentTerminal|preloadAgentTerminal|prepareAgentRuntime|agentRuntime|loadAgentExperience|loadHomeFrame)\b|import\(/,
  );
  assert.match(accountSession, /import \{ Provider, Storage, webAuthn \} from "accounts"/);
  assert.match(managedRuntime, /Agent,[\s\S]*?from "nanocodex\/managed"/);
});

test("the website owns browser Agent creation while the shared terminal owns voice", () => {
  assert.match(terminal, /import \{[\s\S]*?createConfig,[\s\S]*?useNanocodex,[\s\S]*?\} from "nanocodex-react"/);
  assert.match(terminal, /const agentConfig = useMemo\(\(\) => createConfig\(\{/);
  assert.match(
    terminal,
    /useNanocodex\(\{ config: agentConfig, threadId \}\)/,
  );
  assert.match(terminal, /<AgentTerminalView[\s\S]*?voice[\s\S]*?voiceOptions=\{\{ beforeAgentTurn: beforeLocalTurn \}\}/);
  assert.match(terminal, /export const ManagedAgentTerminal[\s\S]*?<AgentTerminalView[\s\S]*?voice/);
  assert.doesNotMatch(terminal, /NanocodexProvider|prepareAgent|preload/);
});

test("signed-out state retains terminal geometry without loading copy", () => {
  const reserve = section(experience, "function ReservedTerminal", "function managedSelectionKey");
  assert.match(reserve, /<TerminalTranscriptSurface/);
  assert.match(reserve, /composer=\{null\}/);
  assert.match(reserve, /mode=\{mode\}/);
  assert.match(terminalCss, /\.agent-terminal-shell\.is-dom \{[\s\S]*?minmax\(0, 1fr\)/);
  assert.match(homeCss, /\.home-page \.agent-terminal-shell \{[\s\S]*?height:\s*100%/);
  assert.doesNotMatch(experience, />\s*(?:loading|spinner|skeleton)/i);
});

test("evaluation query state stays behind the Evals route", () => {
  assert.doesNotMatch(`${entry}\n${application}\n${routeLoaders}`, /from "@tanstack\/react-query"/);
  assert.match(evals, /QueryClientProvider client=\{queryClient\}/);
  assert.match(evals, /export async function preloadEvalOverview/);
  assert.match(routeLoaders, /surface === "evals"[\s\S]*?preloadEvalOverview\(\)/);
});

function source(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}

function section(value: string, start: string, end: string): string {
  const from = value.indexOf(start);
  const to = value.indexOf(end, from);
  assert.notEqual(from, -1, `missing ${start}`);
  assert.notEqual(to, -1, `missing ${end}`);
  return value.slice(from, to);
}
