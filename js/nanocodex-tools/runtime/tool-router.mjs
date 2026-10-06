const TOOL_RESULT = Symbol.for("nanocodex.toolResult");
// Only factory-owned maps have immutable resolver entries. A provider may change
// definitions OR handlers without changing object identity, so it is never cached.
const toolMapSources = new WeakSet();
export const toolRouterBrand = Symbol.for("nanocodex.toolRouter");
export const toolRouterRuntime = Symbol("nanocodex.toolRouterRuntime");
export const toolRuntimeLifecycle = Symbol("nanocodex.toolRuntimeLifecycle");
export const preDispatchUnavailable = Symbol.for("nanocodex.tool.preDispatchUnavailable");

const TOOL_SEARCH_DEFINITION = deepFreeze({
  type: "tool_search",
  execution: "client",
  description: "Searches all deferred tool sources and exposes matching tools for the next model call.",
  parameters: {
    type: "object",
    properties: {
      query: { type: "string", description: "Search query for deferred tools." },
      limit: { type: "number", description: "Maximum number of tools to return. Defaults to 8." },
    },
    required: ["query"],
    additionalProperties: false,
  },
});

/** Deterministic routing and per-call snapshots shared by direct and Code Mode calls. */
export class ToolRouter {
  #sources = new Map();
  #reset;
  #mapEntries = new WeakMap();

  constructor(sources = []) {
    for (const source of sources) this.addSource(source);
  }

  get [toolRouterBrand]() { return true; }

  addSource(source) {
    const normalized = normalizeSource(source);
    if (this.#sources.has(normalized.id)) {
      throw new Error(`tool source is already configured: ${normalized.id}`);
    }
    this.#sources.set(normalized.id, normalized);
    // Build now so collisions fail at configuration, independent of source order.
    try { this.#buildSnapshot(); } catch (error) {
      this.#sources.delete(normalized.id);
      throw error;
    }
    return this;
  }

  /** Validates a candidate source against the complete route table without publishing it. */
  validateSource(source) {
    const normalized = normalizeSource(source);
    const previous = this.#sources.get(normalized.id);
    this.#sources.set(normalized.id, normalized);
    try {
      this.#buildSnapshot();
      return true;
    } finally {
      if (previous) this.#sources.set(normalized.id, previous);
      else this.#sources.delete(normalized.id);
    }
  }

  async attachSource(source) {
    const normalized = normalizeSource({ ...source, kind: "attached", mode: "attached-over-cloud" });
    if (this.#sources.has(normalized.id)) {
      throw new Error(`tool source is already configured: ${normalized.id}`);
    }
    this.#sources.set(normalized.id, normalized);
    try { this.#buildSnapshot(); } catch (error) {
      this.#sources.delete(normalized.id);
      throw error;
    }
  }

  async detachSource(id) {
    return this.#sources.delete(id);
  }

  async admit(signal) {
    signal?.throwIfAborted?.();
    return this.snapshot();
  }

  snapshot() {
    const built = this.#buildSnapshot();
    return Object.freeze({
      definitions: built.definitions,
      tools: built.tools,
      catalog: (provider = "javascript") => catalogSnapshot(built, provider),
      invoke: (name, input, context, observe) => this.#invoke(built, name, input, context, observe),
      endTurn: (sessionId, turnId, event) => this.endTurn(sessionId, turnId, event),
      release() {},
    });
  }

  modelDefinitions() {
    // Provider catalogs (notably MCP discovery) are live until an admission
    // snapshots them. Rebuild here so newly discovered deferred definitions
    // become available to Code Mode; `defer_loading` keeps them out of the
    // direct model prefix at the Rust boundary.
    return this.#buildSnapshot().modelDefinitions;
  }

  definitions() { return this.#buildSnapshot().definitions; }
  resolve(name) { return this.#buildSnapshot().tools.get(name); }
  hasSource(id) { return this.#sources.has(id); }
  hasSourceKind(kind) {
    return [...this.#sources.values()].some((source) => source.kind === kind);
  }

  async settled() {
    const sources = [...this.#sources.values()];
    await Promise.all(sources.map((source) => source.settled?.()));
  }

  catalog(provider = "javascript") {
    return catalogSnapshot(this.#buildSnapshot(), provider);
  }

  async execute(name, input, context) {
    const admission = await this.admit(context?.signal);
    try { return await admission.invoke(name, input, context); } finally { admission.release(); }
  }

  async endTurn(sessionId, turnId, event = "Stop") {
    if (!["Stop", "Interrupt", "SubagentStop"].includes(event)) throw new TypeError("invalid turn lifecycle event");
    const owners = new Set([...this.#sources.values(), ...this.#allTools()]);
    await settleCleanup([...owners].filter(owner => typeof owner.endTurn === "function")
      .map(owner => () => owner.endTurn(sessionId, turnId, event)), "turn cleanup failed");
  }

  releaseSession(sessionId) {
    for (const tool of this.#allTools()) tool.releaseSession?.(sessionId);
  }

  reset(reentrantPromise) {
    if (this.#reset) return this.#reset;
    const sources = [...this.#sources.values()];
    let tools = [];
    let enumerationError;
    try { tools = [...this.#allTools(sources.filter((source) => typeof source.close !== "function"))]; }
    catch (error) { enumerationError = error; }
    this.#sources.clear();
    const actions = [
      ...(enumerationError ? [() => { throw enumerationError; }] : []),
      ...tools.map((tool) => () => tool.dispose?.()),
      ...sources.filter((source) => typeof source.close === "function").map((source) => () => source.close()),
    ];
    this.#reset = Promise.resolve().then(() => settleCleanup(
      actions,
      "tool router cleanup failed",
      reentrantPromise,
    ));
    return this.#reset;
  }

  #allTools(sources = this.#sources.values()) {
    const tools = new Set();
    for (const source of sources) {
      for (const definition of source.definitions()) {
        const name = definition.type === "tool_search" ? "tool_search" : definition.name;
        const tool = source.resolve(name);
        if (tool) tools.add(tool);
      }
    }
    return tools;
  }

  #buildSnapshot() {
    const entries = [];
    const searches = [];
    const sources = [...this.#sources.values()].sort((a, b) => a.id.localeCompare(b.id));
    for (const source of sources) {
      // Keep reading and serializing the public array: callers can mutate it.
      // Reuse only factory-owned contract work when its exact JSON is unchanged.
      let encoded;
      try { encoded = JSON.stringify(source.definitions()); }
      catch (error) { throw new TypeError(`tool source ${source.id} definitions must be JSON-serializable`, { cause: error }); }
      const cached = toolMapSources.has(source) ? this.#mapEntries.get(source) : undefined;
      if (cached && cached.encoded === encoded) {
        entries.push(...cached.entries);
        continue;
      }
      let definitions;
      try { definitions = JSON.parse(encoded); }
      catch (error) { throw new TypeError(`tool source ${source.id} definitions must be JSON-serializable`, { cause: error }); }
      if (!Array.isArray(definitions)) throw new TypeError(`tool source ${source.id} definitions() must return an array`);
      const sourceEntries = [];
      for (const [sourceIndex, raw] of definitions.entries()) {
        if (raw?.type === "tool_search") {
          if (typeof source.search === "function") searches.push({ source, search: source.search });
          continue;
        }
        const candidate = catalogCandidate(raw, source.id);
        const exactDefinition = normalizeDefinition(candidate?.definition ?? raw, source.id);
        const definition = source.kind === "attached"
          ? deepFreeze({ ...exactDefinition, defer_loading: true })
          : exactDefinition;
        const tool = source.resolve(definition.name);
        if (!tool) throw new Error(`tool source ${source.id} cannot resolve ${definition.name}`);
        const resolved = normalizeResolvedTool(definition.name, tool, candidate);
        sourceEntries.push({
          definition,
          fingerprint: undefined,
          normalizedName: normalizeToolName(definition.name),
          source,
          sourceIndex,
          tool: resolved,
        });
      }
      entries.push(...sourceEntries);
      if (toolMapSources.has(source)) this.#mapEntries.set(source, { encoded, entries: sourceEntries });
    }
    entries.sort(compareEntries);
    const selected = new Map();
    const normalized = new Map();
    for (const entry of entries) {
      const collision = normalized.get(entry.normalizedName);
      if (!collision) {
        selected.set(entry.definition.name, entry);
        normalized.set(entry.normalizedName, entry);
        continue;
      }
      const winner = selectCollision(collision, entry);
      selected.delete(collision.definition.name);
      selected.set(winner.definition.name, winner);
      normalized.set(entry.normalizedName, winner);
    }
    const ordered = [...selected.values()].sort((a, b) =>
      a.source.id.localeCompare(b.source.id) || a.sourceIndex - b.sourceIndex);
    const tools = new Map(ordered.map((entry) => [
      entry.definition.name,
      entry.fallback ? overlayTool(entry.tool, entry.fallback.tool) : entry.tool,
    ]));
    const definitions = ordered.map((entry) => entry.definition);
    const hasSearch = searches.length
        || sources.some((source) => source.kind === "attached" || source.kind === "mcp" || source.deferred === true)
        || ordered.some((entry) => entry.definition.defer_loading === true);
    if (hasSearch) {
      definitions.unshift(TOOL_SEARCH_DEFINITION);
      tools.set("tool_search", Object.freeze({
        name: "tool_search",
        parallelSafe: true,
        handler: (input, context) => searchSnapshot(input, context, ordered, searches),
      }));
    }
    const modelDefinitions = [
      ...(hasSearch ? [TOOL_SEARCH_DEFINITION] : []),
      ...ordered.flatMap((entry) => entry.source.kind === "attached"
        ? entry.fallback ? [entry.fallback.definition] : []
        : [entry.definition]),
    ];
    return Object.freeze({
      definitions: deepFreeze(definitions),
      modelDefinitions: deepFreeze(modelDefinitions),
      tools,
    });
  }

  async #invoke(snapshot, name, input, context = {}, observe) {
    const tool = snapshot.tools.get(name);
    if (!tool) throw new Error(`unknown application tool: ${name}`);
    const signal = context.signal ?? new AbortController().signal;
    signal.throwIfAborted?.();
    // Trusted attachment diagnostics are separate from the tool context. A
    // callback receives only fixed local boundaries and cannot change execution.
    try { observe?.("execution_started"); } catch {}
    try { return await tool.handler(input, context); }
    finally { try { observe?.("execution_finished"); } catch {} }
  }
}

export function createToolRouter(sources) { return new ToolRouter(sources); }

/** Runs every cleanup action before reporting failures in their original order. */
export async function settleCleanup(actions, message, reentrantPromise) {
  const pending = actions.map((action) => {
    try {
      const result = action();
      return result === reentrantPromise ? Promise.resolve() : Promise.resolve(result);
    }
    catch (error) { return Promise.reject(error); }
  });
  const settled = await Promise.allSettled(pending);
  const errors = settled
    .filter((result) => result.status === "rejected")
    .map((result) => result.reason);
  if (errors.length === 1) throw errors[0];
  if (errors.length > 1) throw new AggregateError(errors, message);
}

export function toolMapSource(id, configuration = {}, options = {}) {
  const tools = new Map();
  const definitions = [];
  for (const [name, value] of Object.entries(configuration)) {
    if (!value || typeof value.handler !== "function") {
      throw new TypeError(`tool ${name} requires a handler function`);
    }
    const definition = value.definition
      ? { ...value.definition, name }
      : {
          type: "function",
          name,
          description: value.description || "Application-defined tool.",
          strict: value.strict ?? false,
          parameters: value.parameters || { type: "object", additionalProperties: true },
          ...(value.outputSchema !== undefined ? { output_schema: value.outputSchema } : {}),
        };
    definitions.push(deepFreeze(jsonSnapshot(definition, `tool ${name} definition`)));
    tools.set(name, Object.freeze({
      name,
      handler: value.handler,
      parallelSafe: value.supportsParallelToolCalls === true || value.parallelSafe === true,
      provider: value.provider,
      remoteName: value.remoteName,
      summary: value.summary,
      timeoutMs: value.timeoutMs,
      dispose: typeof value.dispose === "function" ? value.dispose : undefined,
      endTurn: typeof value.endTurn === "function" ? (...args) => value.endTurn(...args) : undefined,
      releaseSession: typeof value.releaseSession === "function" ? value.releaseSession : undefined,
    }));
  }
  const source = Object.freeze({
    id,
    kind: options.kind ?? "cloud",
    mode: options.mode ?? "union",
    definitions: () => definitions,
    resolve: (name) => tools.get(name),
  });
  toolMapSources.add(source);
  return source;
}

export function providerSource(id, provider, options = {}) {
  if (!provider || typeof provider.definitions !== "function" || typeof provider.resolve !== "function") {
    throw new TypeError("a tool source requires definitions() and resolve(name)");
  }
  return Object.freeze({
    id,
    kind: options.kind ?? provider.kind ?? "union",
    mode: options.mode ?? provider.mode ?? "union",
    definitions: () => provider.definitions(),
    resolve: (name) => provider.resolve(name),
    search: typeof provider.search === "function" ? (input, context) => provider.search(input, context) : undefined,
    deferred: options.deferred ?? provider.deferred,
    settled: () => provider.settled?.(),
    ...(typeof provider.endTurn === "function" ? { endTurn: (...args) => provider.endTurn(...args) } : {}),
    ...(typeof provider.close === "function" ? { close: () => provider.close() } : {}),
  });
}

export function logicalContractFingerprint(definition) {
  const logical = { ...normalizeDefinition(definition, "fingerprint") };
  // Defer loading changes request presentation, not the callable contract.
  delete logical.defer_loading;
  return stableJson(logical);
}

export function normalizeToolName(name) {
  if (typeof name !== "string" || !name.trim()) throw new TypeError("tool name must not be empty");
  return [...name].map((character) => /[A-Za-z0-9_-]/.test(character) ? character : "_").join("");
}

async function searchSnapshot(input, context, entries, searches) {
  const query = requiredQuery(input?.query);
  const limit = normalizeLimit(input?.limit);
  const admittedNames = new Set(entries.map((entry) => entry.definition.name));
  const searchedSources = new Set(searches.map(({ source }) => source));
  const words = tokenize(query);
  const generic = entries
    .filter((entry) => !searchedSources.has(entry.source)
      && (entry.source.kind === "attached" || entry.definition.defer_loading === true))
    .map((entry) => ({ entry, score: score(entry, words) }))
    .filter(({ score }) => score > 0)
    .sort((a, b) => b.score - a.score || a.entry.definition.name.localeCompare(b.entry.definition.name));
  const tools = generic.map(({ entry }) => ({
    name: entry.definition.name,
    provider: entry.source.id,
    remote_name: entry.source.remoteName?.(entry.definition.name) ?? entry.definition.name,
    description: entry.definition.description ?? "",
    supports_parallel_tool_calls: entry.tool.parallelSafe,
    input_schema: entry.definition.parameters,
    ...(entry.definition.output_schema !== undefined ? { output_schema: entry.definition.output_schema } : {}),
  }));
  const failures = {};
  const providerResults = [];
  let pending = 0;
  for (const { search } of searches) {
    const result = await search({ query, limit }, context);
    const body = result?.[TOOL_RESULT] ? result.output : result;
    if (result?.[TOOL_RESULT]) {
      providerResults.push(...(Array.isArray(result.structuredResult)
        ? result.structuredResult
        : [result.structuredResult]));
    }
    const value = Array.isArray(body) ? body[0] : body;
    if (Array.isArray(value?.tools)) tools.push(...value.tools);
    pending += value?.pending_servers ?? value?.pending_sources ?? 0;
    Object.assign(failures, value?.failed_servers ?? value?.failed_sources ?? {});
  }
  const deduplicated = [...new Map(tools
    .filter((tool) => admittedNames.has(tool.name))
    .map((tool) => [tool.name, tool])).values()].slice(0, limit);
  const selectedNames = new Set(deduplicated.map((tool) => tool.name));
  const structured = [];
  const representedNames = new Set();
  for (const value of providerResults) {
    const admitted = admittedStructuredResult(value, selectedNames, representedNames);
    if (!admitted) continue;
    structured.push(admitted);
    for (const name of structuredToolNames(admitted)) representedNames.add(name);
  }
  structured.push(...generic
    .map(({ entry }) => entry)
    .filter((entry) => selectedNames.has(entry.definition.name)
      && !representedNames.has(entry.definition.name))
    .map((entry) => providerToolDefinition(entry.definition)));
  const result = {
    tools: deduplicated,
    pending_sources: pending,
    failed_sources: failures,
    // Preserve the established MCP result fields during the transition.
    pending_servers: pending,
    failed_servers: failures,
  };
  return Object.freeze({
    [TOOL_RESULT]: true,
    metadata: null,
    output: result,
    structuredResult: structured,
    success: true,
    value: result,
  });
}

function admittedStructuredResult(value, admittedNames, representedNames) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  if (value.type === "function" || value.type === "custom") {
    return admittedNames.has(value.name) && !representedNames.has(value.name)
      ? providerToolDefinition(value)
      : undefined;
  }
  if (value.type !== "namespace" || typeof value.name !== "string" || !Array.isArray(value.tools)) {
    return undefined;
  }
  const namespaceNames = new Set();
  const tools = value.tools
    .filter((tool) => {
      if (typeof tool?.name !== "string") return false;
      const name = `${value.name}${tool.name}`;
      if (!admittedNames.has(name) || representedNames.has(name) || namespaceNames.has(name)) {
        return false;
      }
      namespaceNames.add(name);
      return true;
    })
    .map(providerToolDefinition);
  return tools.length ? deepFreeze({
    type: "namespace",
    name: value.name,
    description: typeof value.description === "string" ? value.description : "",
    tools,
  }) : undefined;
}

function structuredToolNames(value) {
  if (value.type === "namespace") return value.tools.map((tool) => `${value.name}${tool.name}`);
  return [value.name];
}

function providerToolDefinition(definition) {
  if (definition.type === "custom") {
    return deepFreeze({
      type: "custom",
      name: definition.name,
      description: typeof definition.description === "string" ? definition.description : "",
      defer_loading: true,
      format: definition.format,
    });
  }
  return deepFreeze({
    type: "function",
    name: definition.name,
    description: typeof definition.description === "string" ? definition.description : "",
    strict: definition.strict === true,
    defer_loading: true,
    parameters: definition.parameters,
  });
}

function catalogSnapshot(snapshot, provider) {
  return deepFreeze(snapshot.definitions
    .filter((definition) => definition.type === "function" || definition.type === "custom")
    .map((definition) => {
      const tool = snapshot.tools.get(definition.name);
      return {
        provider: tool.provider ?? provider,
        remote_name: tool.remoteName ?? definition.name,
        definition,
        parallel_safe: tool.parallelSafe,
        ...(tool.summary !== undefined ? { summary: tool.summary } : {}),
        ...(tool.timeoutMs !== undefined ? { timeout_ms: tool.timeoutMs } : {}),
      };
    })
    .sort((left, right) => left.definition.name.localeCompare(right.definition.name)));
}

function selectCollision(left, right) {
  const leftAttached = left.source.mode === "attached-over-cloud" || left.source.kind === "attached";
  const rightAttached = right.source.mode === "attached-over-cloud" || right.source.kind === "attached";
  if (leftAttached !== rightAttached) {
    const attached = leftAttached ? left : right;
    const cloud = leftAttached ? right : left;
    if (cloud.source.kind !== "cloud") {
      throw collisionError(left, right, "attached overlays may only replace cloud tools");
    }
    if (attached.definition.name !== cloud.definition.name || entryFingerprint(attached) !== entryFingerprint(cloud)) {
      throw collisionError(left, right, "attached/cloud catalog parity mismatch");
    }
    return { ...attached, fallback: cloud };
  }
  throw collisionError(left, right, left.definition.name === right.definition.name
    ? "duplicate tool name"
    : "normalized tool name collision");
}

function overlayTool(attached, cloud) {
  return Object.freeze({
    ...attached,
    parallelSafe: attached.parallelSafe === true && cloud.parallelSafe === true,
    async handler(input, context) {
      const result = await attached.handler(input, context);
      return result?.[preDispatchUnavailable] === true
        ? cloud.handler(input, context)
        : result;
    },
    async endTurn(...args) { await Promise.all([attached.endTurn?.(...args), cloud.endTurn?.(...args)]); },
    releaseSession(sessionId) {
      attached.releaseSession?.(sessionId);
      cloud.releaseSession?.(sessionId);
    },
    dispose() {
      attached.dispose?.();
      cloud.dispose?.();
    },
  });
}

function collisionError(left, right, reason) {
  return new Error(`${reason}: ${left.source.id}/${left.definition.name} and ${right.source.id}/${right.definition.name}`);
}

function normalizeSource(source) {
  if (!source || typeof source !== "object" || typeof source.id !== "string" || !source.id.trim()) {
    throw new TypeError("tool source requires a non-empty id");
  }
  if (typeof source.definitions !== "function" || typeof source.resolve !== "function") {
    throw new TypeError(`tool source ${source.id} requires definitions() and resolve(name)`);
  }
  const mode = source.mode ?? "union";
  if (mode !== "union" && mode !== "attached-over-cloud") throw new TypeError(`unknown tool source mode: ${mode}`);
  const normalized = Object.freeze({ ...source, id: source.id.trim(), kind: source.kind ?? "union", mode });
  if (toolMapSources.has(source)) toolMapSources.add(normalized);
  return normalized;
}

function normalizeDefinition(definition, source) {
  if (!definition || typeof definition !== "object" || Array.isArray(definition)) {
    throw new TypeError(`tool source ${source} returned a non-object definition`);
  }
  if ((definition.type !== "function" && definition.type !== "custom")
      || typeof definition.name !== "string" || !definition.name.trim()) {
    throw new TypeError(`tool source ${source} returned an invalid callable definition`);
  }
  return deepFreeze(jsonSnapshot(definition, `tool ${definition.name} definition`));
}

function catalogCandidate(value, source) {
  if (value?.definition === undefined) return undefined;
  if (!value || typeof value !== "object" || Array.isArray(value)
      || typeof value.provider !== "string" || !value.provider.trim()
      || typeof value.remote_name !== "string" || !value.remote_name.trim()
      || typeof value.parallel_safe !== "boolean"
      || !Number.isSafeInteger(value.timeout_ms) || value.timeout_ms < 1) {
    throw new TypeError(`tool source ${source} returned an invalid catalog candidate`);
  }
  return value;
}

function normalizeResolvedTool(name, tool, candidate) {
  if (!tool || typeof tool.handler !== "function") throw new TypeError(`resolved tool ${name} requires a handler`);
  return Object.freeze({
    ...tool,
    name,
    parallelSafe: candidate?.parallel_safe
      ?? (tool.parallelSafe === true || tool.supportsParallelToolCalls === true),
    provider: candidate?.provider ?? tool.provider,
    remoteName: candidate?.remote_name ?? tool.remoteName,
    summary: candidate?.summary ?? tool.summary,
    timeoutMs: candidate?.timeout_ms ?? tool.timeoutMs,
  });
}

// Most tools never overlap. Only compute parity for an attached/cloud collision;
// each entry owns an immutable definition, including across admitted snapshots.
function entryFingerprint(entry) {
  return entry.fingerprint ??= callableContractFingerprint(entry.definition);
}

function callableContractFingerprint(definition) {
  const logical = { ...definition };
  delete logical.defer_loading;
  delete logical.description;
  return stableJson(logical);
}

function compareEntries(a, b) {
  return a.normalizedName.localeCompare(b.normalizedName)
    || a.definition.name.localeCompare(b.definition.name)
    || a.source.id.localeCompare(b.source.id);
}

function requiredQuery(value) {
  if (typeof value !== "string" || !value.trim()) throw new TypeError("tool_search query must not be empty");
  return value.trim();
}

function normalizeLimit(value = 8) {
  if (!Number.isInteger(value) || value < 1) throw new TypeError("tool_search limit must be a positive integer");
  return Math.min(value, 32);
}

function tokenize(value) { return value.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean); }
function score(entry, words) {
  const text = `${entry.definition.name} ${entry.definition.description ?? ""} ${Object.keys(entry.definition.parameters?.properties ?? {}).join(" ")}`.toLowerCase();
  return words.reduce((total, word) => total + (text.includes(word) ? 1 : 0), 0);
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function jsonSnapshot(value, label) {
  try { return JSON.parse(JSON.stringify(value)); }
  catch (error) { throw new TypeError(`${label} must be JSON-serializable`, { cause: error }); }
}

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}
