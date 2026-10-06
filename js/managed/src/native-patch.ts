import { patchPaths } from "./claude-engine-runtime.mjs";
import type { NamespaceExecutionRuntime } from "./namespace-tools";
import type { NamedTool, ToolContext } from "nanocodex";
import type { Workspace } from "nanocodex-tools";
import { executeNativePatch } from "./native-patch-runtime.mjs";

/** Use the shipped Rust parser, hunk verifier and mutation plan with durable Brain IO.
 * Each invocation owns a private host route; it never replaces an agent host.
 * The outer Code Mode effect journal owns replay/unknown-outcome fencing.
 */
export function nativeBrainPatch(filesystem: Workspace, prepare: () => Promise<void>, authorize: (context: ToolContext) => void, namespace?: Pick<NamespaceExecutionRuntime, "patch">): NamedTool {
  let pending: Promise<void> = Promise.resolve();
  return {
    name: "apply_patch",
    description: "Apply a patch using the native Rust patch parser and hunk verifier to durable /brain files or exactly one authorized Hand mount. Pass the complete *** Begin Patch / *** End Patch string. Relative paths resolve under /brain. Inspect files after uncertain effects; never automatically retry.",
    parameters: { type: "object", additionalProperties: false },
    handler: async (input, context) => {
      if (typeof input !== "string") throw new TypeError("apply_patch requires a patch string");
      const previous = pending;
      let release!: () => void;
      pending = new Promise<void>(resolve => { release = resolve; });
      await previous;
      const check = () => { authorize(context); context.signal.throwIfAborted(); };
      const host = {
        readWorkspaceFile: async (path: string) => { check(); return filesystem.readFile(path); },
        writeWorkspaceFile: async (path: string, bytes: Uint8Array) => {
          check();
          const slash = path.lastIndexOf("/");
          if (slash > 0) await filesystem.mkdir(path.slice(0, slash));
          check();
          await filesystem.writeFile(path, bytes);
        },
        removeWorkspaceFile: async (path: string) => { check(); await filesystem.remove(path); },
      };
      try {
        check();
        const paths = patchPaths(input);
        const routed = await namespace?.patch(input, paths, context);
        if (routed) return routed.result;
        check(); await prepare(); check();
        return await executeNativePatch(input, host);
      } finally { release(); }
    },
  };
}
