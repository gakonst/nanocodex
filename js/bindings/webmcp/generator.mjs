import { readdir, readFile } from "node:fs/promises";
import { extname, join, relative, resolve, sep } from "node:path";

const SOURCE_EXTENSIONS = new Set([
  ".cjs", ".gql", ".graphql", ".htm", ".html", ".js", ".json",
  ".jsx", ".mjs", ".ts", ".tsx", ".yaml", ".yml",
]);
const IGNORED_DIRECTORIES = new Set([
  ".git", ".next", ".turbo", "build", "coverage", "dist", "node_modules",
  "out", "target", "vendor",
]);
const HTTP_METHODS = new Set(["DELETE", "GET", "HEAD", "PATCH", "POST", "PUT"]);
const DEFAULT_MAX_FILE_BYTES = 1_000_000;
const DEFAULT_MAX_FILES = 5_000;

/**
 * Statically inspects a repository and returns an inert, reviewable WebMCP
 * manifest. Source is read as data and is never imported or executed.
 */
export async function generate(options = {}) {
  validateOptions(options);
  const root = resolve(options.root ?? process.cwd());
  const maxFileBytes = boundedInteger(
    options.maxFileBytes ?? DEFAULT_MAX_FILE_BYTES,
    "maxFileBytes",
    1,
    16_000_000,
  );
  const maxFiles = boundedInteger(options.maxFiles ?? DEFAULT_MAX_FILES, "maxFiles", 1, 100_000);
  const files = await sourceFiles(root, maxFiles);
  const candidates = [];
  for (const path of files) {
    let contents;
    try { contents = await readFile(path, "utf8"); }
    catch (error) {
      if (error?.code === "ERR_STRING_TOO_LONG") continue;
      throw error;
    }
    if (Buffer.byteLength(contents) > maxFileBytes || contents.includes("\u0000")) continue;
    const file = relative(root, path).split(sep).join("/");
    candidates.push(...analyzeFile(file, contents));
  }
  const tools = deduplicate(candidates).map((candidate) => Object.freeze({
    ...candidate,
    approved: false,
  }));
  return deepFreeze({
    version: 1,
    generatedAt: new Date().toISOString(),
    root: ".",
    tools,
  });
}

/** Validates the stable generated-manifest shape without executing handlers. */
export function validate(manifest) {
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)
      || manifest.version !== 1 || !Array.isArray(manifest.tools)) {
    throw new TypeError("WebMCP manifest must use version 1 and contain a tools array");
  }
  const names = new Set();
  for (const [index, tool] of manifest.tools.entries()) {
    if (!tool || typeof tool !== "object" || Array.isArray(tool)
        || typeof tool.name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(tool.name)
        || typeof tool.description !== "string" || !tool.description.trim()
        || typeof tool.approved !== "boolean"
        || !tool.implementation || typeof tool.implementation.kind !== "string"
        || !Array.isArray(tool.evidence) || tool.evidence.length === 0) {
      throw new TypeError(`WebMCP manifest tool ${index} is invalid`);
    }
    if (names.has(tool.name)) throw new Error(`duplicate WebMCP manifest tool: ${tool.name}`);
    names.add(tool.name);
  }
  return true;
}

function analyzeFile(file, contents) {
  const tools = [];
  const extension = extname(file).toLowerCase();
  if (extension === ".json") tools.push(...openApiTools(file, contents));
  if (extension === ".html" || extension === ".htm" || /<form\b/i.test(contents)) {
    tools.push(...formTools(file, contents));
  }
  if (/\b(?:query|mutation)\s+[A-Za-z_][A-Za-z0-9_]*/.test(contents)) {
    tools.push(...graphqlTools(file, contents));
  }
  if (/\buse server\b/.test(contents)) tools.push(...serverActionTools(file, contents));
  tools.push(...nextRouteTools(file, contents));
  tools.push(...routerTools(file, contents));
  tools.push(...fetchTools(file, contents));
  tools.push(...trpcTools(file, contents));
  return tools;
}

function nextRouteTools(file, contents) {
  const match = file.match(/(?:^|\/)app\/(api\/.*?)\/route\.[cm]?[jt]sx?$/);
  if (!match) return [];
  const path = `/${match[1]}`.replace(/\/\([^/]+\)/g, "");
  const tools = [];
  const methodPattern = /export\s+(?:async\s+)?function\s+(DELETE|GET|HEAD|PATCH|POST|PUT)\b/g;
  for (const result of contents.matchAll(methodPattern)) {
    tools.push(fetchCandidate(file, lineNumber(contents, result.index), result[1], path, "Next.js route"));
  }
  return tools;
}

function routerTools(file, contents) {
  const tools = [];
  const pattern = /\b(?:api|app|fastify|router|server)\s*\.\s*(delete|get|head|patch|post|put)\s*\(\s*(["'`])([^"'`]+)\2/g;
  for (const result of contents.matchAll(pattern)) {
    const method = result[1].toUpperCase();
    const path = result[3];
    if (!path.startsWith("/")) continue;
    tools.push(fetchCandidate(file, lineNumber(contents, result.index), method, path, "application route"));
  }
  return tools;
}

function fetchTools(file, contents) {
  const tools = [];
  const pattern = /\bfetch\s*\(\s*(["'`])([^"'`$]+)\1/g;
  for (const result of contents.matchAll(pattern)) {
    const path = result[2];
    if (!path.startsWith("/") || path.startsWith("//")) continue;
    const tail = contents.slice(result.index + result[0].length, result.index + result[0].length + 1_500);
    const method = tail.match(/^\s*,\s*\{[\s\S]{0,1400}?\bmethod\s*:\s*["'`](DELETE|GET|HEAD|PATCH|POST|PUT)["'`]/i)?.[1]?.toUpperCase()
      ?? "GET";
    tools.push(fetchCandidate(file, lineNumber(contents, result.index), method, path, "frontend fetch"));
  }
  return tools;
}

function formTools(file, contents) {
  const tools = [];
  const pattern = /<form\b([^>]*)>([\s\S]*?)<\/form\s*>/gi;
  let ordinal = 0;
  for (const result of contents.matchAll(pattern)) {
    ordinal += 1;
    const attributes = htmlAttributes(result[1]);
    const action = attributes.action || "";
    const method = (attributes.method || "get").toUpperCase();
    const formName = attributes.id || attributes.name || pathName(action) || `form_${ordinal}`;
    const selector = attributes.id
      ? `#${cssIdentifier(attributes.id)}`
      : attributes.name ? `form[name="${cssString(attributes.name)}"]`
        : action ? `form[action="${cssString(action)}"]` : `form:nth-of-type(${ordinal})`;
    const properties = {};
    const required = [];
    const fieldPattern = /<(?:input|select|textarea)\b([^>]*)>/gi;
    for (const field of result[2].matchAll(fieldPattern)) {
      const fieldAttributes = htmlAttributes(field[1]);
      if (!fieldAttributes.name || fieldAttributes.type === "hidden") continue;
      properties[fieldAttributes.name] = {
        type: fieldAttributes.type === "checkbox" ? "boolean" : "string",
      };
      if (Object.hasOwn(fieldAttributes, "required")) required.push(fieldAttributes.name);
    }
    tools.push(candidate({
      name: `form_${formName}`,
      title: humanize(formName),
      description: `Submit the ${humanize(formName)} form through the website's current authenticated page.`,
      inputSchema: {
        type: "object",
        properties: {
          fields: {
            type: "object",
            properties,
            ...(required.length ? { required } : {}),
            additionalProperties: Object.keys(properties).length === 0,
          },
        },
        required: ["fields"],
        additionalProperties: false,
      },
      annotations: { readOnlyHint: false, untrustedContentHint: false },
      implementation: { kind: "form", selector },
      evidence: evidence(file, contents, result.index, `HTML form ${method} ${action || "current URL"}`),
    }));
  }
  return tools;
}

function graphqlTools(file, contents) {
  const tools = [];
  const pattern = /\b(query|mutation)\s+([A-Za-z_][A-Za-z0-9_]*)\b/g;
  for (const result of contents.matchAll(pattern)) {
    const operation = result[1];
    const name = result[2];
    tools.push(candidate({
      name: `graphql_${name}`,
      title: humanize(name),
      description: `Run the ${name} GraphQL ${operation} using the website's authenticated GraphQL client.`,
      inputSchema: openObject("variables"),
      annotations: { readOnlyHint: operation === "query", untrustedContentHint: true },
      implementation: { kind: "custom", export: name, operation },
      evidence: evidence(file, contents, result.index, `GraphQL ${operation} ${name}`),
    }));
  }
  return tools;
}

function serverActionTools(file, contents) {
  const tools = [];
  const patterns = [
    /export\s+(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(/g,
    /export\s+const\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*async\s*\(/g,
  ];
  for (const pattern of patterns) {
    for (const result of contents.matchAll(pattern)) {
      const name = result[1];
      tools.push(candidate({
        name: `action_${name}`,
        title: humanize(name),
        description: `Run the website server action ${name} through an application-supplied handler.`,
        inputSchema: openObject("input"),
        annotations: { readOnlyHint: false, untrustedContentHint: false },
        implementation: { kind: "custom", export: name },
        evidence: evidence(file, contents, result.index, `server action ${name}`),
      }));
    }
  }
  return tools;
}

function trpcTools(file, contents) {
  const tools = [];
  const pattern = /\b([A-Za-z_$][A-Za-z0-9_$]*)\s*:\s*[^,;{}]{0,400}?\.(query|mutation)\s*\(/g;
  for (const result of contents.matchAll(pattern)) {
    const name = result[1];
    const operation = result[2];
    tools.push(candidate({
      name: `trpc_${name}`,
      title: humanize(name),
      description: `Call the website tRPC ${operation} ${name} through its application client.`,
      inputSchema: openObject("input"),
      annotations: { readOnlyHint: operation === "query", untrustedContentHint: operation === "query" },
      implementation: { kind: "custom", procedure: name, operation },
      evidence: evidence(file, contents, result.index, `tRPC ${operation} ${name}`),
    }));
  }
  return tools;
}

function openApiTools(file, contents) {
  let document;
  try { document = JSON.parse(contents); } catch { return []; }
  if (!document || typeof document !== "object" || typeof document.openapi !== "string"
      || !document.paths || typeof document.paths !== "object") return [];
  const tools = [];
  for (const [path, item] of Object.entries(document.paths)) {
    if (!item || typeof item !== "object") continue;
    for (const [methodName, operation] of Object.entries(item)) {
      const method = methodName.toUpperCase();
      if (!HTTP_METHODS.has(method) || !operation || typeof operation !== "object") continue;
      const name = operation.operationId || `${method.toLowerCase()}_${pathName(path) || "root"}`;
      tools.push(candidate({
        ...fetchCandidate(file, 1, method, path, "OpenAPI operation"),
        name,
        title: operation.summary || humanize(name),
        description: operation.description || operation.summary || `${method} ${path}`,
        evidence: [{ file, line: 1, kind: `OpenAPI ${method} ${path}` }],
      }));
    }
  }
  return tools;
}

function fetchCandidate(file, line, method, path, kind) {
  const readOnly = method === "GET" || method === "HEAD";
  const name = `${method.toLowerCase()}_${pathName(path) || "root"}`;
  const pathParameters = [...path.matchAll(/\[([^\]]+)\]|:([A-Za-z0-9_]+)/g)]
    .map((match) => match[1] ?? match[2]);
  return candidate({
    name,
    title: `${method} ${path}`,
    description: `${method} ${path} through the website's same-origin authenticated session.`,
    inputSchema: {
      type: "object",
      properties: {
        ...(pathParameters.length ? {
          path: {
            type: "object",
            properties: Object.fromEntries(pathParameters.map((parameter) => [parameter, { type: "string" }])),
            required: pathParameters,
            additionalProperties: false,
          },
        } : {}),
        query: { type: "object", additionalProperties: true },
        ...(!readOnly ? { body: {} } : {}),
      },
      ...(pathParameters.length ? { required: ["path"] } : {}),
      additionalProperties: false,
    },
    annotations: { readOnlyHint: readOnly, untrustedContentHint: true },
    implementation: { kind: "fetch", method, path },
    evidence: [{ file, line, kind: `${kind} ${method} ${path}` }],
  });
}

function candidate(tool) {
  return {
    ...tool,
    name: safeName(tool.name),
  };
}

function deduplicate(candidates) {
  const exact = new Map();
  for (const tool of candidates) {
    const key = JSON.stringify(tool.implementation);
    const existing = exact.get(key);
    if (!existing) exact.set(key, tool);
    else existing.evidence.push(...tool.evidence);
  }
  const byName = new Map();
  const tools = [];
  for (const tool of exact.values()) {
    let name = tool.name;
    const collision = byName.get(name);
    if (collision && JSON.stringify(collision.implementation) !== JSON.stringify(tool.implementation)) {
      name = `${name.slice(0, 119)}_${shortHash(JSON.stringify(tool.implementation))}`;
    }
    const next = { ...tool, name };
    byName.set(name, next);
    tools.push(next);
  }
  return tools.sort((left, right) => left.name.localeCompare(right.name));
}

async function sourceFiles(root, maximum) {
  const files = [];
  const pending = [root];
  while (pending.length) {
    const directory = pending.pop();
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (!IGNORED_DIRECTORIES.has(entry.name) && !entry.name.startsWith(".wrangler")) pending.push(path);
        continue;
      }
      if (!entry.isFile() || !SOURCE_EXTENSIONS.has(extname(entry.name).toLowerCase())) continue;
      files.push(path);
      if (files.length > maximum) throw new RangeError(`WebMCP generator exceeded ${maximum} source files`);
    }
  }
  return files.sort();
}

function htmlAttributes(source) {
  const attributes = {};
  const pattern = /([:\w-]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g;
  for (const result of source.matchAll(pattern)) {
    attributes[result[1].toLowerCase()] = result[2] ?? result[3] ?? result[4] ?? "";
  }
  return attributes;
}

function evidence(file, contents, index, kind) {
  return [{ file, line: lineNumber(contents, index), kind }];
}

function lineNumber(contents, index = 0) {
  return contents.slice(0, index).split("\n").length;
}

function openObject(name) {
  return {
    type: "object",
    properties: { [name]: { type: "object", additionalProperties: true } },
    additionalProperties: false,
  };
}

function pathName(path) {
  return String(path)
    .replace(/^https?:\/\/[^/]+/i, "")
    .replace(/\?.*$/, "")
    .replace(/\[|\]|:/g, "")
    .split(/[^A-Za-z0-9_$]+/)
    .filter(Boolean)
    .join("_")
    .slice(0, 96);
}

function safeName(value) {
  const normalized = String(value)
    .split("")
    .map((character) => /[A-Za-z0-9_.-]/.test(character) ? character : "_")
    .join("")
    .replace(/_+/g, "_")
    .replace(/^[_\-.]+|[_\-.]+$/g, "") || "tool";
  return normalized.length <= 128
    ? normalized
    : `${normalized.slice(0, 119)}_${shortHash(normalized)}`;
}

function humanize(value) {
  return String(value)
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_\-.]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function cssIdentifier(value) {
  return String(value).replace(/[^A-Za-z0-9_-]/g, (character) => `\\${character}`);
}

function cssString(value) {
  return String(value).replace(/["\\]/g, "\\$&");
}

function shortHash(value) {
  let hash = 0x811c9dc5;
  for (const character of value) {
    hash ^= character.codePointAt(0);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36).padStart(7, "0").slice(0, 7);
}

function validateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("WebMCP generator options must be an object");
  }
  const allowed = new Set(["maxFileBytes", "maxFiles", "root"]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported WebMCP generator option: ${name}`);
  }
}

function boundedInteger(value, name, minimum, maximum) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new TypeError(`${name} must be an integer from ${minimum} through ${maximum}`);
  }
  return value;
}

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}
