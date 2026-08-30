import { watch } from "node:fs";
import { extname, relative, resolve, sep } from "node:path";

import { generateFile } from "../webmcp/generator.mjs";

const watchers = new Map();

/**
 * Wraps a Next.js config with build/startup WebMCP manifest generation.
 * Development servers also keep the manifest current as source files change.
 */
export function withWebMcp(nextConfig = {}, options = {}) {
  validateOptions(options);
  if (typeof nextConfig !== "function"
      && (!nextConfig || typeof nextConfig !== "object" || Array.isArray(nextConfig))) {
    throw new TypeError("withWebMcp requires a Next.js config object or function");
  }
  return async function nanocodexNextConfig(phase, context) {
    const resolved = typeof nextConfig === "function"
      ? await nextConfig(phase, context)
      : nextConfig;
    const generation = manifestOptions(options);
    const result = await generateFile(generation);
    if (result.changed) {
      console.info(
        `[nanocodex] generated ${relative(generation.root, result.path)} (${result.manifest.tools.length} review-required tools)`,
      );
    }
    if (phase === "phase-development-server") watchManifest(generation);
    return resolved;
  };
}

function manifestOptions(options) {
  const root = resolve(options.root ?? process.cwd());
  return {
    ...options,
    root,
    output: options.output ?? "webmcp.manifest.json",
  };
}

function watchManifest(options) {
  const output = resolve(options.root, options.output);
  if (watchers.has(output)) return;
  let timer;
  let running;
  let queued = false;
  const update = () => {
    if (running) {
      queued = true;
      return running;
    }
    running = generateFile(options).then((result) => {
      if (result.changed) {
        console.info(
          `[nanocodex] generated ${relative(options.root, result.path)} (${result.manifest.tools.length} review-required tools)`,
        );
      }
    }).catch((error) => {
      console.error(`[nanocodex] WebMCP generation failed: ${errorMessage(error)}`);
    }).finally(() => {
      running = undefined;
      if (queued) {
        queued = false;
        void update();
      }
    });
    return running;
  };
  const watcher = watch(options.root, { recursive: true }, (_event, filename) => {
    if (!filename) return;
    const path = resolve(options.root, String(filename));
    const local = relative(options.root, path);
    if (path === output || local.startsWith(`..${sep}`)
        || ignored(local) || !SOURCE_EXTENSIONS.has(extname(path).toLowerCase())) return;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      void update();
    }, 50);
  });
  watcher.unref();
  watchers.set(output, watcher);
}

function ignored(path) {
  return path.split(/[\\/]/).some((part) => IGNORED_DIRECTORIES.has(part));
}

function validateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("withWebMcp options must be an object");
  }
  const allowed = new Set(["maxFileBytes", "maxFiles", "output", "root"]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported withWebMcp option: ${name}`);
  }
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

const SOURCE_EXTENSIONS = new Set([
  ".cjs", ".gql", ".graphql", ".htm", ".html", ".js", ".json",
  ".jsx", ".mjs", ".ts", ".tsx", ".yaml", ".yml",
]);
const IGNORED_DIRECTORIES = new Set([
  ".git", ".next", ".turbo", "__fixtures__", "__tests__", "build", "coverage",
  "dist", "fixtures", "node_modules", "out", "target", "test", "tests", "vendor",
]);
