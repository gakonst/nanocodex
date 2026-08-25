import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { browserMcpConfiguration } from "../src/browserMcp.ts";

const terminal = source("../src/AgentTerminal.tsx");
const connectTerminal = source("../connect-playground/src/ConnectAgentExperience.tsx");
const terminalView = source("../../js/terminal/src/AgentTerminalView.tsx");
const dock = source("../src/ArtifactDock.tsx");
const terminalCss = [
  source("../src/AgentTerminal.css"),
  source("../../js/terminal/styles.css"),
].join("\n");
const experience = source("../src/AgentExperience.tsx");
const viteConfig = source("../vite.config.ts");

test("one thread-scoped Config supplies clone-safe MCP servers to the retained Agent", () => {
  const declaration = section(terminal, "export const AgentTerminal", "export const ManagedAgentTerminal");
  assert.equal(matches(terminal, /createConfig\(/g), 1);
  assert.match(declaration, /useMemo\(\(\) => createConfig\(\{[\s\S]*?mcp: browserMcpConfiguration\(location\.origin, threadId\)[\s\S]*?durability: false[\s\S]*?\[threadId\]/);
  assert.match(
    terminal,
    /useNanocodex\(\{ config: agentConfig, threadId \}\)/,
  );
  assert.doesNotMatch(terminal, /prepareAgentRuntime|NanocodexProvider/);

  const configuration = browserMcpConfiguration(
    "https://agent.test/path",
    "11111111-1111-4111-8111-111111111111",
  );
  assert.deepEqual(structuredClone(configuration), configuration);
  assert.ok(Object.values(configuration).every((server) =>
    typeof server.url === "string"
    && Array.isArray(server.enabledTools)
    && Object.values(server.headers).every((value) => typeof value === "string")));
});

test("the landing terminal is ephemeral while the Agent demo is managed-durable only", () => {
  const declaration = section(terminal, "export const AgentTerminal", "export const ManagedAgentTerminal");
  assert.match(declaration, /durability: false/);
  assert.doesNotMatch(declaration, /localTerminalAgent|\bdurable\b/);
  assert.match(experience, /activeCapabilityError = landing \? capabilityError : undefined/);
  assert.match(experience, /landing[\s\S]*?\? hasCredential[\s\S]*?<AgentTerminal[\s\S]*?: hasCredential && managedConversationId[\s\S]*?<ManagedAgentTerminal/);
  assert.match(experience, /runtime="managed"/);
  assert.doesNotMatch(experience, /activeRuntime|agent-runtime-switch|Local browser|Managed durable|localConversations/);
});

test("the Connect terminal enables package-owned voice from final-output permission", () => {
  assert.match(connectTerminal, /<AgentTerminalView[\s\S]*?voice=\{visibility\.finalMessages\}/);
});

test("the full Agent experience alone mounts a collapsed, counted artifact dock", () => {
  assert.match(terminalView, /return mode === "full" \? \([\s\S]*?className="agent-terminal-workspace"[\s\S]*?accessory\?\.\([\s\S]*?\) : terminal/);
  const local = section(terminal, "export const AgentTerminal", "export const ManagedAgentTerminal");
  const managed = section(terminal, "export const ManagedAgentTerminal", "function artifactFollowOnPrompt");
  assert.equal(matches(local, /accessory=\{/g), 1);
  assert.equal(matches(local, /<ArtifactDock\b/g), 1);
  assert.equal(matches(managed, /accessory=\{/g), 1);
  assert.equal(matches(managed, /<ArtifactDock\b/g), 1);
  assert.match(dock, /const \[collapsed, setCollapsed\] = useState\(true\)/);
  assert.match(dock, /aria-expanded=\{false\}/);
  assert.match(dock, /aria-label=\{`Open artifacts, \$\{artifactCount\}`\}/);
  assert.match(dock, /<span aria-hidden="true">\{artifacts\.length\}<\/span>/);
  assert.match(dock, /const expand = useCallback\(\(\) => \{[\s\S]*?setCollapsed\(false\)/);
  assert.match(dock, /onClick=\{expand\}/);
  assert.match(dock, /label="Collapse artifacts"[\s\S]*?onClick=\{collapse\}/);
  assert.match(dock, /subscribeThreadWorkspaceChanges\([\s\S]*?getBrowserThread\(\)\.id,[\s\S]*?refresh\(store\)/);

  assert.match(ruleBlock(terminalCss, ".nanocodex-demo.is-full .agent-terminal-workspace > .agent-terminal-shell {"), /height:\s*100%/);
  assert.match(ruleBlock(terminalCss, ".nanocodex-demo.is-full .artifact-dock {"), /position:\s*absolute/);
  const toggle = ruleBlock(terminalCss, ".nanocodex-demo.is-full .artifact-dock.is-collapsed > button {");
  assert.match(toggle, /min-width:\s*44px/);
  assert.match(toggle, /min-height:\s*44px/);
});

test("an artifact action queues exactly one contextual follow-on on the retained terminal", () => {
  const ask = section(dock, "const ask =", "const createExample =");
  assert.equal(matches(ask, /onPrompt\(/g), 1);
  assert.match(ask, /onPrompt\(selected, prompt, store\.path\(selected\.id\)\)/);

  const submit = section(terminalView, "const submitAccessoryPrompt =", "const terminal =");
  assert.equal(matches(submit, /controller\.submit\(/g), 1);
  assert.match(submit, /retainSubmittedPrompt\(submittedPrompts\.current, input, submittedAt\)/);
  assert.match(submit, /controller\.submit\(input, \{ intent: "queue" \}\)/);
  assert.equal(matches(terminal, /artifactFollowOnPrompt\(artifact, path, prompt\)/g), 2);
  const contextualPrompt = section(terminal, "function artifactFollowOnPrompt", "function errorMessage");
  assert.match(contextualPrompt, /JSON\.stringify\(artifact\.id\)/);
  assert.match(contextualPrompt, /JSON\.stringify\(path\)/);
  assert.match(contextualPrompt, /prompt\.trim\(\)/);
  const surfaceSource = `${terminal}\n${terminalView}\n${dock}`;
  assert.doesNotMatch(surfaceSource, /NanocodexTui|Artifact action queued|spinner|skeleton/i);
  assert.doesNotMatch(surfaceSource, /[">]Loading(?:[ .<]|$)/i);
});

test("credential-gated terminal uses the normal static Vite graph", () => {
  assert.match(viteConfig, /optimizeDeps:\s*\{\s*exclude: \["nanocodex", "nanocodex-react"\]/);
  assert.doesNotMatch(viteConfig, /optimizeDeps:[\s\S]*?include:/);
  assert.match(
    experience,
    /import \{ AgentTerminal, ManagedAgentTerminal \} from "\.\/AgentTerminal"/,
  );
  assert.doesNotMatch(experience, /import\(|\blazy\b|loadAgentTerminal|preloadAgentTerminal|prepareAgentRuntime/);
});

test("managed conversation selection is invalidated when account ownership changes", () => {
  assert.match(
    experience,
    /useEffect\(\(\) => \{\s*setManagedConversations\(\[\]\);\s*setManagedConversationId\(undefined\);\s*setRuntimeState\(undefined\);\s*\}, \[account\.account\?\.id\]\)/,
  );
  assert.match(experience, /managedSelectionKey\(accountId\)/);
});

test("managed startup mounts a retained selection before refreshing the rail", () => {
  assert.match(
    experience,
    /const retainedId = safeGet\(managedSelectionKey\(accountId\)\) \?\? undefined;\s*setManagedConversationId\(retainedId\);\s*setConversationPending\(true\);[\s\S]*?listManagedConversations\(accountId\)\.then\(async \(listed\) => \{\s*if \(cancelled\) return;\s*const next = listed\.length \|\| !hasCredential \? listed : \[await createManagedConversation\(accountId\)\]/,
  );
});

test("managed list failures expose a real retry action", () => {
  assert.match(experience, /const \[managedAttempt, setManagedAttempt\] = useState\(0\)/);
  assert.match(experience, /setManagedAttempt\(\(value\) => value \+ 1\)/);
  assert.match(experience, /onRetry=\{retryManagedConversations\}/);
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

function ruleBlock(css: string, selector: string): string {
  const start = css.indexOf(selector);
  assert.notEqual(start, -1, `missing ${selector}`);
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  return css.slice(start, close + 1);
}

function matches(value: string, pattern: RegExp) {
  return [...value.matchAll(pattern)].length;
}
