import type { ManagedAgent } from "nanocodex/managed";
import type { Workspace } from "nanocodex/browser/workspace";
import { createTools } from "nanocodex/tools";
import { browser as createBrowserRuntime } from "nanocodex/tools/browser";

const BROWSER_HAND_TOOL_NAMES = new Set([
  "exec_command",
  "render_artifact",
  "view_image",
]);
const BROWSER_HAND_CAPABILITIES = Object.freeze([
  "browser",
  "wasm",
  "filesystem",
  "shell",
  "git",
  "artifacts",
  "images",
]);

let browserHandIdentity: Readonly<{ id: string; workspaceId: string }> | undefined;

export type ManagedBrowserHand = Readonly<{
  close(): Promise<void>;
  closed(): Promise<void>;
  workspace: Workspace;
  workspaceId: string;
}>;

/** Attaches this page's OPFS/WASM runtime as one explicit hand of a managed brain. */
export async function attachManagedBrowserHand(
  agent: ManagedAgent,
  signal: AbortSignal,
): Promise<ManagedBrowserHand> {
  signal.throwIfAborted();
  const { id, workspaceId } = currentBrowserHandIdentity();
  const browser = await createBrowserRuntime({
    installFetch: false,
    origin: window.location.origin,
    threadId: workspaceId,
  });
  signal.throwIfAborted();
  const localTools = browser.tools.filter(({ name }) => BROWSER_HAND_TOOL_NAMES.has(name));
  if (localTools.length !== BROWSER_HAND_TOOL_NAMES.size) {
    throw new Error("the browser runtime did not provide its complete local tool catalog");
  }
  const tools = await createTools({
    attachmentId: id,
    machines: [{
      id,
      name: `Web client ${id.slice(-6)}`,
      workspace: "/workspace",
      capabilities: BROWSER_HAND_CAPABILITIES,
    }],
    tools: localTools,
    workspace: browser.filesystem,
  });
  const abort = () => { void tools.close(); };
  signal.addEventListener("abort", abort, { once: true });
  try {
    signal.throwIfAborted();
    const client = await tools.attach(agent.toolsTarget()).connect();
    signal.throwIfAborted();
    return Object.freeze({
      close: () => tools.close(),
      closed: () => client.closed(),
      workspace: browser.filesystem,
      workspaceId,
    });
  } catch (error) {
    await tools.close().catch(() => {});
    throw error;
  } finally {
    signal.removeEventListener("abort", abort);
  }
}

function currentBrowserHandIdentity(): Readonly<{ id: string; workspaceId: string }> {
  browserHandIdentity ??= Object.freeze((() => {
    const workspaceId = crypto.randomUUID();
    return { id: `web-${workspaceId}`, workspaceId };
  })());
  return browserHandIdentity;
}
