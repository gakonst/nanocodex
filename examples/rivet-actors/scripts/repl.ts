import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";

import { createNanocodexClient } from "../src/client.js";

type PendingTurn = { id: string; input: string };
type ReplState = {
  endpoint: string;
  session_key: string;
  pending?: PendingTurn;
};

const endpoint = process.env.RIVET_PUBLIC_ENDPOINT ?? "http://127.0.0.1:6420";
const sessionKey = process.env.NANOCODEX_REPL_SESSION ?? "repl";
const statePath = resolve(process.env.NANOCODEX_REPL_STATE ?? ".nanocodex/rivet-repl.json");
const client = createNanocodexClient(endpoint);
const session = client.nanocodex.getOrCreate([sessionKey]);
const loadedState = await loadState();
let interrupted = false;
let input: ReturnType<typeof createInterface> | undefined;

if (loadedState && (loadedState.endpoint !== endpoint || loadedState.session_key !== sessionKey)) {
  throw new Error(
    `${statePath} belongs to ${loadedState.endpoint}/${loadedState.session_key}; set NANOCODEX_REPL_STATE to use another session`,
  );
}
const state: ReplState = loadedState ?? { endpoint, session_key: sessionKey };
await saveState();

const detach = () => {
  if (interrupted) return;
  interrupted = true;
  input?.close();
  process.stderr.write(state.pending
    ? "\nDetached. Re-run the REPL to resume this turn.\n"
    : "\nREPL closed.\n");
  process.exit(130);
};
process.once("SIGINT", detach);

try {
  const status = await session.status();
  process.stdout.write(
    `Nanocodex Rivet REPL (${sessionKey}, ${status.completed_turns} committed turns)\n`,
  );
  if (state.pending) await completePending(state.pending, true);

  input = createInterface({ input: process.stdin, output: process.stdout, prompt: "nanocodex> " });
  input.on("SIGINT", detach);
  input.prompt();
  for await (const line of input) {
    const prompt = line.trim();
    if (!prompt) {
      input.prompt();
      continue;
    }
    if (prompt === "/exit" || prompt === "/quit") break;
    if (prompt === "/status") {
      process.stdout.write(`${JSON.stringify(await session.status())}\n`);
      input.prompt();
      continue;
    }

    const pending = { id: randomUUID(), input: prompt };
    state.pending = pending;
    await saveState();
    await completePending(pending, false);
    if (interrupted) break;
    input.prompt();
  }
} catch (error) {
  if (!interrupted) throw error;
} finally {
  input?.close();
}

async function completePending(pending: PendingTurn, resumed: boolean): Promise<void> {
  const accepted = await session.start(pending);
  process.stderr.write(
    `${resumed ? "Resuming" : "Started"} ${pending.id}${accepted.replayed ? " (already running)" : ""}. Ctrl-C detaches.\n`,
  );
  const completed = await session.turn(pending);
  if (state.pending?.id === pending.id) {
    delete state.pending;
    await saveState();
  }
  process.stdout.write(`${completed.final_message}\n`);
}

async function loadState(): Promise<ReplState | undefined> {
  try {
    const parsed = JSON.parse(await readFile(statePath, "utf8")) as ReplState;
    if (typeof parsed.endpoint !== "string" || typeof parsed.session_key !== "string") {
      throw new Error("missing endpoint or session key");
    }
    if (parsed.pending && (
      typeof parsed.pending.id !== "string" || typeof parsed.pending.input !== "string"
    )) {
      throw new Error("invalid pending turn");
    }
    return parsed;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw new Error(`cannot read ${statePath}: ${errorMessage(error)}`);
  }
}

async function saveState(): Promise<void> {
  await mkdir(dirname(statePath), { recursive: true, mode: 0o700 });
  const temporary = `${statePath}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify(state)}\n`, { mode: 0o600 });
  await rename(temporary, statePath);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
