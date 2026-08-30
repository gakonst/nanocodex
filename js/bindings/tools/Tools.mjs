import { resolveTools } from "../runtime/tool-configuration.mjs";
import { tools as workspaceTools } from "../runtime/workspace.mjs";
import {
  providerSource,
  settleCleanup,
  ToolRouter,
  toolMapSource,
  toolRouterBrand,
  toolRouterRuntime,
  toolRuntimeLifecycle,
} from "../runtime/tool-router.mjs";
import { createAttachment } from "./attachment.mjs";

/**
 * Builds one language-neutral JavaScript-owned tool runtime.
 * Empty custom tools, no workspace, and no MCP are the defaults.
 */
export async function createTools(options = {}) {
  validateOptions(options);
  const router = new ToolRouter();
  const resolved = resolveTools(
    options.tools === undefined ? {} : options.tools,
    { defaultSubagents: false },
  );
  if (resolved.subagents) throw new TypeError("createTools does not accept agent-relative extensions");
  const custom = resolved.tools;
  const providerIds = [];
  let mcp;
  try {
    if (Object.keys(custom).length) {
      router.addSource(toolMapSource("custom", custom, { kind: "cloud" }));
    }
    if (options.workspace !== undefined) {
      router.addSource(toolMapSource(
        "workspace",
        workspaceTools(options.workspace, options.workspaceOptions),
        { kind: "cloud" },
      ));
    }
    for (const [index, provider] of (options.providers ?? []).entries()) {
      const sourceId = provider.sourceId ?? `provider:${String(index).padStart(8, "0")}`;
      router.addSource(providerSource(
        sourceId,
        provider,
        {
          kind: provider.kind ?? "union",
          mode: provider.mode ?? "union",
          deferred: provider.deferred,
        },
      ));
      providerIds.push(sourceId);
    }
    if (options.mcp !== undefined && options.mcp !== false) {
      const { createMcpRuntime } = await import("../runtime/mcp-runtime.mjs");
      mcp = await createMcpRuntime(options.mcp, options.mcpOptions);
      router.addSource(providerSource("mcp", mcp, { kind: "mcp" }));
    }
    await Promise.all((options.providers ?? []).map((provider) => provider.settled?.()));
  } catch (error) {
    const cleanup = [
      ...providerIds.map((sourceId) => router.detachSource(sourceId)),
      ...(mcp ? [mcp.close()] : []),
    ];
    const failures = (await Promise.allSettled(cleanup))
      .filter((result) => result.status === "rejected")
      .map((result) => result.reason);
    if (failures.length) {
      throw new AggregateError(
        [error, ...failures],
        "Tools construction and provider cleanup failed",
      );
    }
    throw error;
  }
  const attachments = new Set();
  let closed = false;
  let closing;
  let claimed = false;
  let owner;
  const lifecycle = Object.freeze({
    available() {
      if (closed) throw new Error("Tools runtime is closed");
      if (claimed) throw new Error("Tools runtime already belongs to an Agent host");
    },
    claim() {
      this.available();
      claimed = true;
    },
    close() { return owner.close(); },
  });
  owner = {
    [toolRouterBrand]: true,
    [toolRouterRuntime]: router,
    [toolRuntimeLifecycle]: lifecycle,
    attach(target) {
      if (closed) throw new Error("Tools runtime is closed");
      if (arguments.length !== 1) throw new TypeError("Tools.attach accepts only a target");
      const attachment = createAttachment(owner, target);
      attachments.add(attachment);
      void attachment.closed().then(() => attachments.delete(attachment));
      return attachment;
    },
    close() {
      if (closing) return closing;
      closed = true;
      const ownedAttachments = [...attachments];
      attachments.clear();
      closing = Promise.resolve().then(async () => {
        const errors = [];
        try {
          await settleCleanup(
            ownedAttachments.map((attachment) => () => attachment.close()),
            "tool attachment cleanup failed",
          );
        } catch (error) {
          errors.push(...cleanupErrors(error));
        }
        try { await router.reset(closing); }
        catch (error) { errors.push(...cleanupErrors(error)); }
        if (errors.length === 1) throw errors[0];
        if (errors.length > 1) throw new AggregateError(errors, "Tools cleanup failed");
      });
      return closing;
    },
  };
  return Object.freeze(owner);
}

function cleanupErrors(error) {
  return error instanceof AggregateError ? error.errors : [error];
}

function validateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("createTools options must be an object");
  }
  const allowed = new Set(["tools", "workspace", "workspaceOptions", "mcp", "mcpOptions", "providers"]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported createTools option: ${name}`);
  }
  if (options.workspace === undefined && options.workspaceOptions !== undefined) {
    throw new TypeError("workspaceOptions requires workspace");
  }
  if ((options.mcp === undefined || options.mcp === false) && options.mcpOptions !== undefined) {
    throw new TypeError("mcpOptions requires mcp");
  }
  if (options.providers !== undefined && !Array.isArray(options.providers)) {
    throw new TypeError("providers must be an array");
  }
}
