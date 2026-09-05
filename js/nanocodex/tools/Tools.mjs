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
import { normalizeHostedMachines } from "nanocodex-tools/internal/hosted-machine";

/**
 * Builds one language-neutral JavaScript-owned tool runtime.
 * Empty custom tools, no workspace, and no MCP are the defaults.
 */
export async function createTools(options = {}) {
  validateOptions(options);
  const machines = normalizeHostedMachines(options.machines);
  const attachmentId = options.attachmentId;
  if (machines.length > 0 && attachmentId !== machines[0].id) {
    throw new TypeError("createTools machine attachment requires one machine whose id equals attachmentId");
  }
  const router = new ToolRouter();
  const resolved = resolveTools(
    options.tools === undefined ? {} : options.tools,
    { defaultSubagents: false },
  );
  if (resolved.subagents) throw new TypeError("createTools does not accept agent-relative extensions");
  const custom = resolved.tools;
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
    if (options.mcp !== undefined && options.mcp !== false) {
      const { createMcpRuntime } = await import("../runtime/mcp-runtime.mjs");
      mcp = await createMcpRuntime(options.mcp, options.mcpOptions);
      router.addSource(providerSource("mcp", mcp, { kind: "mcp" }));
    }
  } catch (error) {
    if (!mcp) throw error;
    try { await mcp.close(); }
    catch (cleanupError) {
      throw new AggregateError(
        [error, cleanupError],
        "Tools construction and MCP cleanup failed",
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
      const attachment = createAttachment(owner, target, { machines, attachmentId });
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
  const allowed = new Set(["tools", "workspace", "workspaceOptions", "mcp", "mcpOptions", "machines", "attachmentId"]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported createTools option: ${name}`);
  }
  if (options.attachmentId !== undefined && (typeof options.attachmentId !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,122}$/.test(options.attachmentId))) {
    throw new TypeError("createTools attachmentId must be a safe identifier of at most 123 bytes");
  }
  if (options.workspace === undefined && options.workspaceOptions !== undefined) {
    throw new TypeError("workspaceOptions requires workspace");
  }
  if ((options.mcp === undefined || options.mcp === false) && options.mcpOptions !== undefined) {
    throw new TypeError("mcpOptions requires mcp");
  }
}
