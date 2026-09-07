import { execFile } from "node:child_process";
import { homedir, tmpdir } from "node:os";
import { isAbsolute, join, relative, resolve } from "node:path";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { promisify } from "node:util";

import {
  Agent,
  ChatGptSubscription,
  Transport,
  createMemoryChatGptSubscriptionStore,
} from "../node/index.mjs";
import { artifact as artifactTool } from "../tools/index.mjs";
import { readCodexSubscription } from "../../managed/scripts/codex-auth-file.mjs";

const executeFile = promisify(execFile);
const authPath = resolve(
  process.env.NANOCODEX_CODEX_AUTH_FILE ?? join(homedir(), ".codex", "auth.json"),
);
const imported = await readCodexSubscription(authPath);
const seed = {
  accessToken: imported.accessToken,
  accountId: imported.accountId,
  fedramp: imported.fedramp,
};
const workspace = await mkdtemp(join(tmpdir(), "nanocodex-code-mode-stress-"));
const calls = [];
let active = 0;
let peak = 0;
let waited = false;
let agent;
let subscription;
let watcher;
let unwatch;

try {
  const subscriptionId = `code-mode-stress-${crypto.randomUUID()}`;
  subscription = await ChatGptSubscription.open({
    id: subscriptionId,
    store: createMemoryChatGptSubscriptionStore(subscriptionId),
    seed,
  });
  agent = await Agent.create({
    transport: Transport.chatGpt({ subscription }),
    workspace: workspace,
    instructions: [
      "You are exercising a browser-computer-compatible Code Mode boundary.",
      "Perform real work through tools.exec_command; never simulate command output.",
      "Keep all file operations and process activity within the supplied workspace; do not search or manage the host system.",
      "Use one elaborate JavaScript cell when the operations can be composed there.",
    ].join(" "),
    tools: [{
      name: "exec_command",
      supportsParallelToolCalls: true,
      description: "Run a bash command in the persistent workspace.",
      parameters: {
        type: "object",
        properties: {
          cmd: { type: "string" },
          workdir: { type: "string" },
        },
        required: ["cmd"],
        additionalProperties: false,
      },
      outputSchema: {
        type: "object",
        properties: {
          wall_time_seconds: { type: "number" },
          exit_code: { type: "number" },
          original_token_count: { type: "number" },
          output: { type: "string", description: "Command output text, possibly truncated." },
        },
        required: ["wall_time_seconds", "output"],
        additionalProperties: false,
      },
      handler: executeCommand,
    }, artifactTool({
      workspace: nodeArtifactWorkspace(),
      validateSource: validateArtifactSource,
    })],
  });
  watcher = agent.events.watch();
  unwatch = watcher.onEvent((event) => {
    if (event.type === "tool.call" && /(?:^|[._])wait$/.test(event.payload?.tool ?? "")) waited = true;
    if (event.type === "tool.call" || event.type === "tool.result" || event.type.startsWith("run.")) {
      process.stderr.write(`${JSON.stringify({
        sequence: event.sequence,
        type: event.type,
        status: event.payload?.status,
        tool: event.payload?.tool,
        ...(event.type === "tool.result" && event.payload?.status !== "completed"
          ? { result: event.payload?.result }
          : {}),
      })}\n`);
    }
  });

  const turn = agent.turn.prompt({
    input: [
      "Stress-test Code Mode end to end and leave a real published artifact.",
      'Start your JavaScript exec cell with // @exec: {"yield_time_ms": 0}, then start three independent exec_command writes with Promise.all:',
      "twenty.txt=20, twenty-one.txt=21, and one.txt=1.",
      "Then read them with a second Promise.all, use map/filter/reduce to compute 42, and store the summary.",
      "Create executable JavaScript defining function App({ sendPrompt }) with the provided html tagged-template helper; JSX is unavailable.",
      "Render the computed total in that component and publish it in the same cell with tools.render_artifact using id stress-ui and title Code Mode Stress.",
      "Use wait to resume the yielded cell until it completes. Finally verify the artifact JSON and report the total. Do not fake any command or artifact output.",
    ].join(" "),
  });
  const result = await resultBeforeDeadline(turn, 90_000);
  turn.dispose();
  const artifactPath = join(workspace, ".nanocodex", "artifacts", "stress-ui.json");
  const artifact = JSON.parse(await readFile(artifactPath, "utf8"));
  validateArtifactSource(artifact.source);
  if (!artifact.source.includes("42")) throw new Error("published artifact does not contain 42");
  if (peak < 3) throw new Error(`exec_command Promise.all only reached concurrency ${peak}`);
  if (!waited) throw new Error("the model did not resume its cell through wait");
  const failed = calls.filter((call) => call.exitCode !== 0);
  process.stdout.write(`${JSON.stringify({
    artifact: artifact.id,
    calls: calls.length,
    failedCalls: failed.map(({ cmd, exitCode }) => ({ cmd, exitCode })),
    finalMessage: result.finalMessage,
    peakConcurrency: peak,
    resumedCell: waited,
    total: 42,
  })}\n`);
} finally {
  unwatch?.();
  watcher?.off();
  await agent?.session.shutdown().catch(() => {});
  subscription?.dispose();
  await rm(workspace, { recursive: true, force: true });
}

async function executeCommand(input, context) {
  if (!input || typeof input !== "object" || typeof input.cmd !== "string" || !input.cmd.trim()) {
    throw new TypeError("exec_command.cmd must be a non-empty string");
  }
  const cwd = workspacePath(input.workdir ?? workspace);
  const startedAt = performance.now();
  const call = { cmd: input.cmd, exitCode: undefined };
  calls.push(call);
  active += 1;
  peak = Math.max(peak, active);
  process.stderr.write(`${JSON.stringify({ active, call: calls.length, phase: "start" })}\n`);
  try {
    await abortableDelay(75, context.signal);
    try {
      const { stdout, stderr } = await executeFile("/bin/bash", ["-c", input.cmd], {
        cwd,
        encoding: "utf8",
        maxBuffer: 4 * 1024 * 1024,
        signal: context.signal,
      });
      call.exitCode = 0;
      return commandResult(`${stdout}${stderr}`, 0, startedAt);
    } catch (error) {
      if (context.signal.aborted) throw context.signal.reason ?? error;
      const exitCode = Number.isInteger(error?.code) ? error.code : 1;
      call.exitCode = exitCode;
      return commandResult(`${error?.stdout ?? ""}${error?.stderr ?? error?.message ?? error}`, exitCode, startedAt);
    }
  } finally {
    active -= 1;
    process.stderr.write(`${JSON.stringify({ active, call: calls.indexOf(call) + 1, phase: "done" })}\n`);
  }
}

function commandResult(output, exitCode, startedAt) {
  return {
    output,
    exit_code: exitCode,
    wall_time_seconds: (performance.now() - startedAt) / 1_000,
  };
}

function validateArtifactSource(source) {
  try {
    new Function(
      "React",
      "html",
      "sendPrompt",
      `"use strict";\n${source}\n;return typeof App === "function" ? App : undefined;`,
    );
  } catch (error) {
    throw new Error(`artifact source is not executable JavaScript: ${error.message}`);
  }
}

function nodeArtifactWorkspace() {
  return {
    root: workspace,
    async list() { return []; },
    async readFile(path) { return readFile(workspacePath(path)); },
    async writeFile(path, contents) { await writeFile(workspacePath(path), contents); },
    async remove(path) { await rm(workspacePath(path), { recursive: true, force: true }); },
    async mkdir(path) { await mkdir(workspacePath(path), { recursive: true }); },
  };
}

function workspacePath(path) {
  const logical = path === workspace || path.startsWith(`${workspace}/`)
    ? relative(workspace, path)
    : path;
  const target = isAbsolute(logical) ? resolve(logical) : resolve(workspace, logical);
  const child = relative(workspace, target);
  if (child.startsWith("..") || isAbsolute(child)) throw new Error("workdir escapes the workspace");
  return target;
}

function abortableDelay(milliseconds, signal) {
  return new Promise((resolveDelay, reject) => {
    const complete = () => {
      signal.removeEventListener("abort", abort);
      resolveDelay();
    };
    const timeout = setTimeout(complete, milliseconds);
    const abort = () => {
      clearTimeout(timeout);
      signal.removeEventListener("abort", abort);
      reject(signal.reason ?? new Error("command cancelled"));
    };
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort();
  });
}

async function resultBeforeDeadline(turn, milliseconds) {
  let timeout;
  try {
    return await Promise.race([
      turn.result(),
      new Promise((_, reject) => {
        timeout = setTimeout(async () => {
          await turn.cancel().catch(() => {});
          reject(new Error(`agent turn exceeded ${milliseconds} milliseconds`));
        }, milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}
