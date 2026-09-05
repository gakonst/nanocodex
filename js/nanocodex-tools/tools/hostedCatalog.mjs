import { resolveTools } from "../runtime/tool-configuration.mjs";
import { ToolRouter, toolMapSource } from "../runtime/tool-router.mjs";

const DEFAULT_TIMEOUT_MS = 120_000;
const CATALOG_DIGEST_DOMAIN = "nanocodex-app-tool-catalog-v1\0";

/** Materializes the exact reverse-tool catalog emitted for app-local named tools. */
export function hostedAppToolCatalog(tools, provider = "javascript") {
  const resolved = resolveTools(tools ?? [], { defaultSubagents: false });
  if (resolved.subagents) throw new TypeError("hosted app tool catalogs do not accept subagents");
  const router = new ToolRouter();
  if (Object.keys(resolved.tools).length > 0) {
    router.addSource(toolMapSource("custom", resolved.tools, { kind: "cloud" }));
  }
  return hostedCatalog(router.catalog(provider));
}

/** Normalizes an admitted router catalog to the socket-owned wire contract. */
export function hostedCatalog(catalog) {
  if (!Array.isArray(catalog) || catalog.length > 256) {
    throw new RangeError("tool attachment catalogs contain at most 256 tools");
  }
  return Object.freeze(catalog.map((entry) => Object.freeze({
    provider: entry.provider,
    remote_name: entry.remote_name,
    definition: hostedDefinition(entry.definition),
    parallel_safe: entry.parallel_safe === true,
    ...(entry.summary === undefined ? {} : { summary: entry.summary }),
    timeout_ms: entry.timeout_ms ?? DEFAULT_TIMEOUT_MS,
  })));
}

/** Returns the exact domain-separated digest signed into a Connect grant. */
export async function hostedToolCatalogDigest(catalog) {
  const normalized = hostedCatalog(catalog);
  const canonical = canonicalJson([...normalized]
    .sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right))));
  const bytes = new TextEncoder().encode(`${CATALOG_DIGEST_DOMAIN}${canonical}`);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return `0x${[...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

function hostedDefinition(definition) {
  if (definition.type === "custom") return Object.freeze({
    type: "custom", name: definition.name, description: definition.description, format: definition.format,
  });
  return Object.freeze({
    type: "function",
    name: definition.name,
    description: definition.description ?? "Application-defined tool.",
    strict: definition.strict ?? false,
    parameters: definition.parameters ?? { type: "object", additionalProperties: true },
    ...(definition.output_schema === undefined ? {} : { output_schema: definition.output_schema }),
  });
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => (
      `${JSON.stringify(key)}:${canonicalJson(value[key])}`
    )).join(",")}}`;
  }
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new TypeError("hosted tool catalogs must be JSON-serializable");
  return encoded;
}
