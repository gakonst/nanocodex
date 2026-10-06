import type { ToolContext } from "nanocodex";
import { resolveNamespaceCwd, type Workspace } from "nanocodex-tools";
import { filePlan, fileSchemas, type FileSnapshot } from "./claude-engine-runtime.mjs";
import type { NamespaceExecutionRuntime } from "./namespace-tools";

export function claudeFiles(options: { filesystem: Workspace; prepareFilesystem?: () => Promise<void>; namespace?: Pick<NamespaceExecutionRuntime, "workspaceTool">; authorize(context: ToolContext): void }) {
  let pending = Promise.resolve();
  return fileSchemas().map(definition => ({name: definition.name, description: definition.description, inputSchema: definition.input_schema,
    handler: async (raw: unknown, context: ToolContext) => {
      const prior = pending;
      let release!: () => void;
      pending = new Promise<void>(resolve => {release = resolve;});
      await prior;
      try {
        const check = () => {options.authorize(context); context.signal.throwIfAborted();};
        check();
        if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("expected tool object");
        const input = raw as Record<string, unknown>;
        const key = definition.name === "NotebookEdit" ? "notebook_path" : ["Glob", "Grep"].includes(definition.name) ? "path" : "file_path";
        const supplied = input[key];
        if (supplied !== undefined && typeof supplied !== "string") throw new Error(`${key} must be a string`);
        if (typeof supplied === "string" && (supplied.split("/").includes("..") || /[\u0000-\u001f]/.test(supplied))) throw new Error("invalid native workspace path");
        const path = resolveNamespaceCwd("/brain", supplied as string | undefined);
        if (path !== "/brain" && !path.startsWith("/brain/")) {
          if (!options.namespace) throw new Error("native Hand file tools unavailable");
          return options.namespace.workspaceTool(definition.name, input, key, context);
        }
        await options.prepareFilesystem?.(); check();
        const fs = options.filesystem;
        const files: FileSnapshot[] = [];
        const directories: string[] = [];
        const relative = (path: string) => {
          if (path === "/brain") return "";
          if (!path.startsWith("/brain/")) throw new Error("file plan escaped Brain mount");
          return path.slice(7);
        };
        const search = ["Glob", "Grep"].includes(definition.name);
        if (search) {
          // Workspace listing validates every entry and rejects symlinks before IO.
          let entries;
          if (path === "/brain") entries = await fs.list(path, {recursive: true, maxEntries: 10000});
          else {
            const parent = path.slice(0, path.lastIndexOf("/"));
            const target = (await fs.list(parent, {maxEntries: 10000})).find(entry => entry.path === path);
            check();
            if (!target) throw new Error("path does not exist");
            if (target.kind === "directory") {
              directories.push(relative(path));
              entries = await fs.list(path, {recursive: true, maxEntries: 9999});
            } else entries = [target];
          }
          check();
          for (const entry of entries) {
            if (entry.kind === "directory") directories.push(relative(entry.path));
            else files.push({path: relative(entry.path), size: entry.size, modified: entry.modifiedAt});
          }
        } else if (definition.name !== "Write") {
          if (definition.name === "Read" && (input.pages !== undefined || /\.pdf$/i.test(path))) throw new Error("PDF Read requires a native media-capable Hand; use a path on an attached Hand mount");
          const bytes = await fs.readFile(path); check();
          if (definition.name === "Read") {
            if (input.pages !== undefined || /\.pdf$/i.test(path) || new TextDecoder().decode(bytes.slice(0, 5)) === "%PDF-") throw new Error("PDF Read requires a native media-capable Hand; use a path on an attached Hand mount");
            const mime = bytes[0] === 137 && bytes[1] === 80 && bytes[2] === 78 && bytes[3] === 71 ? "image/png"
              : bytes[0] === 255 && bytes[1] === 216 ? "image/jpeg"
              : new TextDecoder().decode(bytes.slice(0, 6)).startsWith("GIF8") ? "image/gif"
              : new TextDecoder().decode(bytes.slice(0, 4)) === "RIFF" && new TextDecoder().decode(bytes.slice(8, 12)) === "WEBP" ? "image/webp" : undefined;
            if (mime) {
              if (bytes.byteLength > 5 * 1048576) throw new Error("image exceeds 5 MiB; select a native Hand");
              let binary = "";
              for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
              return {content: [{type: "image", source: {type: "base64", media_type: mime, data: btoa(binary)}}], isError: false};
            }
          }
          if (bytes.byteLength > 1048576) throw new Error("native file read exceeds 1 MiB; select a native Hand for media");
          files.push({path: relative(path), size: bytes.byteLength, content: new TextDecoder("utf-8", {fatal: true}).decode(bytes)});
        }
        const request = {root: "/brain", name: definition.name, input, files, directories, visits: files.length + directories.length};
        if (definition.name === "Grep") {
          const preparation = filePlan({...request, prepare: true});
          let scanned = 0;
          for (const read of preparation.reads) {
            const entry = files.find(file => file.path === read);
            if (!entry) throw new Error("native planner requested a file outside snapshot");
            scanned += Math.min(entry.size ?? 1048577, 1048577);
            if (scanned > 128 * 1048576) throw new Error("native search exceeds 128 MiB scan limit");
            if ((entry.size ?? 0) > 1048576) continue;
            check(); const bytes = await fs.readFile(`/brain/${read}`); check();
            scanned += Math.max(0, Math.min(bytes.byteLength, 1048577) - Math.min(entry.size ?? 1048577, 1048577));
            if (scanned > 128 * 1048576) throw new Error("native search exceeds 128 MiB scan limit");
            entry.size = bytes.byteLength;
            if (bytes.byteLength > 1048576 || bytes.includes(0)) continue;
            try { entry.content = new TextDecoder("utf-8", {fatal: true}).decode(bytes); } catch { /* Binary candidates retain metadata. */ }
          }
        }
        const plan = filePlan(request);
        for (const mutation of plan.mutations) {
          const target = resolveNamespaceCwd("/brain", mutation.path);
          relative(target); check();
          if (mutation.before !== null) {
            const current = new TextDecoder("utf-8", {fatal: true}).decode(await fs.readFile(target)); check();
            if (current !== mutation.before) throw new Error("file changed after native plan; no mutation applied");
          }
          await fs.mkdir(target.slice(0, target.lastIndexOf("/"))); check();
          await fs.writeFile(target, mutation.content); check();
        }
        return {content: plan.output, isError: false};
      } finally {release();}
    }
  }));
}
