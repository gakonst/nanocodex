const PROVIDER_BRAND = Symbol.for("nanocodex.webmcp.provider");
const DEFAULT_MAX_ELEMENTS = 256;
const DEFAULT_MAX_TEXT_CHARS = 20_000;
const DEFAULT_SOURCE_ID = "webmcp:document";
const FALLBACK_NAMES = new Set([
  "web_page_observe",
  "web_page_activate",
  "web_page_fill",
  "web_page_submit",
]);

/**
 * Creates a live tool provider over the current document's WebMCP registry.
 * When the browser has no WebMCP implementation, the bounded semantic fallback
 * keeps the visible page usable without exposing arbitrary JavaScript execution.
 */
export async function createProvider(options = {}) {
  validateProviderOptions(options);
  const document = options.document ?? globalThis.document;
  if (!document || typeof document !== "object") {
    throw new Error("WebMCP requires a browser Document");
  }
  const nativeMode = options.native ?? true;
  const fallbackMode = normalizeFallback(options.fallback);
  const sourceId = nonEmptyString(options.sourceId) ?? DEFAULT_SOURCE_ID;
  const approval = createApproval(document, options);
  const fromOrigins = normalizeOrigins(options.fromOrigins);
  const maxElements = boundedInteger(
    options.maxElements ?? DEFAULT_MAX_ELEMENTS,
    "maxElements",
    1,
    1_024,
  );
  const maxTextChars = boundedInteger(
    options.maxTextChars ?? DEFAULT_MAX_TEXT_CHARS,
    "maxTextChars",
    1,
    100_000,
  );
  const listeners = new Set();
  const nativeTools = new Map();
  const semantic = createSemanticPageTools(document, {
    approval,
    maxElements,
    maxTextChars,
  });
  let definitions = Object.freeze([]);
  let fingerprint = "[]";
  let closed = false;
  let refreshing;
  let stopNativeChange = () => {};

  const provider = {
    [PROVIDER_BRAND]: true,
    sourceId,
    kind: "webmcp",
    mode: "union",
    deferred: true,
    definitions: () => definitions,
    resolve(name) {
      if (closed) return undefined;
      const native = nativeTools.get(name);
      if (native) {
        return Object.freeze({
          name,
          parallelSafe: native.annotations?.readOnlyHint === true,
          handler: (input, context) => executeNativeTool(
            document,
            native,
            input,
            context,
            approval,
          ),
        });
      }
      return semantic.resolve(name);
    },
    settled: () => refresh(),
    refresh,
    subscribe(listener) {
      if (typeof listener !== "function") {
        throw new TypeError("WebMCP provider subscribe requires a listener");
      }
      if (closed) return () => {};
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    close() {
      if (closed) return;
      closed = true;
      stopNativeChange();
      listeners.clear();
      nativeTools.clear();
      semantic.close();
      definitions = Object.freeze([]);
    },
  };

  await refresh();
  const modelContext = document.modelContext;
  if (nativeMode !== false && modelContext?.addEventListener) {
    const changed = () => { void refresh().catch(reportError); };
    modelContext.addEventListener("toolchange", changed);
    stopNativeChange = () => modelContext.removeEventListener?.("toolchange", changed);
  } else if (nativeMode !== false && "ontoolchange" in (modelContext ?? {})) {
    const previous = modelContext.ontoolchange;
    const changed = (event) => {
      if (typeof previous === "function") previous.call(modelContext, event);
      void refresh().catch(reportError);
    };
    modelContext.ontoolchange = changed;
    stopNativeChange = () => {
      if (modelContext.ontoolchange === changed) modelContext.ontoolchange = previous;
    };
  }
  return Object.freeze(provider);

  async function refresh() {
    if (closed) return;
    if (refreshing) return refreshing;
    refreshing = Promise.resolve().then(async () => {
      const modelContext = document.modelContext;
      let registered = [];
      if (nativeMode !== false && typeof modelContext?.getTools === "function") {
        registered = fromOrigins === undefined
          ? await modelContext.getTools()
          : await modelContext.getTools({ fromOrigins });
        if (!Array.isArray(registered)) {
          throw new TypeError("document.modelContext.getTools() returned a non-array");
        }
      } else if (nativeMode === "require") {
        throw new Error("this browser does not provide document.modelContext.getTools()");
      }
      const mirrored = mirrorNativeTools(registered);
      nativeTools.clear();
      for (const entry of mirrored) nativeTools.set(entry.definition.name, entry.tool);
      const includeFallback = fallbackMode === "always"
        || (fallbackMode === "when-empty" && mirrored.length === 0);
      const next = Object.freeze([
        ...mirrored.map((entry) => entry.definition),
        ...(includeFallback ? semantic.definitions() : []),
      ]);
      const nextFingerprint = JSON.stringify(next);
      definitions = next;
      if (nextFingerprint !== fingerprint) {
        fingerprint = nextFingerprint;
        for (const listener of listeners) listener(next);
      }
    }).finally(() => { refreshing = undefined; });
    return refreshing;
  }
}

/** Returns true for providers constructed by createProvider(). */
export function isProvider(value) {
  return value?.[PROVIDER_BRAND] === true
    && typeof value.definitions === "function"
    && typeof value.resolve === "function";
}

/**
 * Registers approved generated tools with the page's native WebMCP registry.
 * The generated manifest remains inert until each tool is explicitly approved.
 */
export async function publish(manifest, options = {}) {
  validatePublishOptions(options);
  const document = options.document ?? globalThis.document;
  const modelContext = document?.modelContext;
  if (typeof modelContext?.registerTool !== "function") {
    throw new Error("publishing WebMCP tools requires document.modelContext.registerTool()");
  }
  if (!manifest || typeof manifest !== "object" || !Array.isArray(manifest.tools)) {
    throw new TypeError("WebMCP manifest requires a tools array");
  }
  const exposedTo = normalizeOrigins(options.exposedTo);
  const registrations = [];
  const names = new Set();
  try {
    for (const candidate of manifest.tools) {
      if (candidate?.approved !== true) continue;
      const tool = validateGeneratedTool(candidate);
      if (names.has(tool.name)) throw new Error(`duplicate approved WebMCP tool: ${tool.name}`);
      names.add(tool.name);
      const controller = new AbortController();
      const execute = generatedExecute(tool, document, options);
      registrations.push({ controller, name: tool.name });
      await modelContext.registerTool({
        name: tool.name,
        title: tool.title ?? tool.name,
        description: tool.description,
        inputSchema: tool.inputSchema,
        annotations: {
          readOnlyHint: tool.annotations?.readOnlyHint === true,
          untrustedContentHint: tool.annotations?.untrustedContentHint === true,
        },
        execute,
      }, {
        signal: controller.signal,
        ...(exposedTo === undefined ? {} : { exposedTo }),
      });
    }
  } catch (error) {
    for (const registration of registrations) registration.controller.abort();
    throw error;
  }
  let closed = false;
  return Object.freeze({
    tools: Object.freeze(registrations.map(({ name }) => name)),
    close() {
      if (closed) return;
      closed = true;
      for (const registration of registrations) registration.controller.abort();
    },
  });
}

function mirrorNativeTools(registered) {
  const prepared = registered.map((tool, index) => {
    if (!tool || typeof tool !== "object"
        || typeof tool.name !== "string" || !tool.name.trim()
        || typeof tool.description !== "string" || !tool.description.trim()) {
      throw new TypeError(`WebMCP tool ${index} is malformed`);
    }
    const origin = typeof tool.origin === "string" ? tool.origin : "same-origin";
    return { key: `${origin}\u0000${tool.name}`, origin, tool };
  }).sort((left, right) => left.key.localeCompare(right.key));
  const baseCounts = new Map();
  for (const entry of prepared) {
    const base = mirroredName(entry.tool.name);
    baseCounts.set(base, (baseCounts.get(base) ?? 0) + 1);
  }
  return prepared.map(({ origin, tool }) => {
    const base = mirroredName(tool.name);
    const name = baseCounts.get(base) === 1
      ? base
      : `${base.slice(0, 51)}_${shortHash(`${origin}\u0000${tool.name}`)}`;
    const description = `${tool.description.trim()} (WebMCP: ${origin})`;
    return Object.freeze({
      tool,
      definition: deepFreeze({
        type: "function",
        name,
        description,
        strict: false,
        defer_loading: true,
        parameters: jsonSchema(tool.inputSchema),
      }),
    });
  });
}

async function executeNativeTool(document, tool, input, context, approval) {
  context?.signal?.throwIfAborted?.();
  const exactInput = cloneApprovalValue(input && typeof input === "object" ? input : {});
  const encodedInput = JSON.stringify(exactInput);
  const execute = async () => {
    context?.signal?.throwIfAborted?.();
    const result = await document.modelContext.executeTool(
      tool,
      encodedInput,
      context?.signal === undefined ? undefined : { signal: context.signal },
    );
    return parseStructuredResult(result);
  };
  if (tool.annotations?.readOnlyHint !== true) {
    return approval.execute({
      kind: "webmcp",
      name: tool.name,
      title: tool.title,
      description: tool.description,
      origin: tool.origin,
      input: exactInput,
      readOnly: false,
    }, execute);
  }
  return execute();
}

function createSemanticPageTools(document, options) {
  const elements = new Map();
  const ids = new WeakMap();
  let nextId = 1;
  let closed = false;
  const definitions = Object.freeze([
    definition(
      "web_page_observe",
      "Read the current page and return its visible text, forms, and actionable elements. Password values are never returned.",
      {
        type: "object",
        properties: {},
        additionalProperties: false,
      },
    ),
    definition(
      "web_page_activate",
      "Activate one visible link, button, or control returned by web_page_observe.",
      {
        type: "object",
        properties: { id: { type: "string", description: "Opaque element ID from web_page_observe." } },
        required: ["id"],
        additionalProperties: false,
      },
    ),
    definition(
      "web_page_fill",
      "Fill visible form controls returned by web_page_observe without submitting the form.",
      {
        type: "object",
        properties: {
          values: {
            type: "array",
            items: {
              type: "object",
              properties: {
                id: { type: "string" },
                value: { type: ["string", "boolean", "number", "null"] },
              },
              required: ["id", "value"],
              additionalProperties: false,
            },
            maxItems: 128,
          },
        },
        required: ["values"],
        additionalProperties: false,
      },
    ),
    definition(
      "web_page_submit",
      "Submit one visible form returned by web_page_observe using the website's normal authenticated page flow.",
      {
        type: "object",
        properties: { id: { type: "string", description: "Opaque form ID from web_page_observe." } },
        required: ["id"],
        additionalProperties: false,
      },
    ),
  ]);

  return Object.freeze({
    definitions: () => definitions,
    resolve(name) {
      if (closed || !FALLBACK_NAMES.has(name)) return undefined;
      if (name === "web_page_observe") {
        return Object.freeze({ name, parallelSafe: true, handler: observe });
      }
      if (name === "web_page_activate") {
        return Object.freeze({ name, parallelSafe: false, handler: activate });
      }
      if (name === "web_page_fill") {
        return Object.freeze({ name, parallelSafe: false, handler: fill });
      }
      return Object.freeze({ name, parallelSafe: false, handler: submit });
    },
    close() { closed = true; elements.clear(); },
  });

  function observe() {
    requireOpen();
    elements.clear();
    const actionable = queryAll(document, [
      "a[href]",
      "button",
      "input",
      "select",
      "textarea",
      "[contenteditable='true']",
      "[role='button']",
      "[role='link']",
      "[role='checkbox']",
      "[role='switch']",
    ].join(","))
      .filter((element) => visible(element))
      .slice(0, options.maxElements)
      .map((element) => describeElement(element));
    const forms = queryAll(document, "form")
      .filter((element) => visible(element))
      .slice(0, options.maxElements)
      .map((form) => ({
        id: retain(form),
        role: "form",
        name: accessibleName(form) || form.getAttribute?.("name") || "form",
        action: form.getAttribute?.("action") || document.location?.href || "",
        method: (form.getAttribute?.("method") || "get").toLowerCase(),
      }));
    const text = String(document.body?.innerText ?? document.documentElement?.innerText ?? "")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, options.maxTextChars);
    return deepFreeze({
      url: document.location?.href ?? "",
      title: document.title ?? "",
      text,
      truncated: text.length === options.maxTextChars,
      elements: actionable,
      forms,
    });
  }

  async function activate(input, context) {
    const element = requiredElement(input?.id);
    return options.approval.execute({
      kind: "semantic",
      name: "web_page_activate",
      input,
      element: describeElement(element),
      readOnly: false,
    }, () => {
      context?.signal?.throwIfAborted?.();
      if (typeof element.click !== "function") throw new Error("page element cannot be activated");
      element.click();
      return { activated: input.id, url: document.location?.href ?? "" };
    });
  }

  async function fill(input, context) {
    requireOpen();
    if (!Array.isArray(input?.values) || input.values.length > 128) {
      throw new TypeError("web_page_fill values must be an array of at most 128 entries");
    }
    const changes = input.values.map((entry) => ({
      element: requiredElement(entry?.id),
      id: entry?.id,
      value: entry?.value,
    }));
    return options.approval.execute({
      kind: "semantic",
      name: "web_page_fill",
      input,
      element: {
        controls: changes.map(({ element, id }) => ({
          id,
          name: accessibleName(element),
          type: String(element.getAttribute?.("type") ?? element.type ?? ""),
        })),
      },
      readOnly: false,
    }, () => {
      const changed = [];
      for (const entry of changes) {
        context?.signal?.throwIfAborted?.();
        setControlValue(entry.element, entry.value, document);
        changed.push(entry.id);
      }
      return { filled: changed };
    });
  }

  async function submit(input, context) {
    const form = requiredElement(input?.id);
    if (String(form.tagName ?? "").toLowerCase() !== "form") {
      throw new TypeError("web_page_submit requires a form ID");
    }
    return options.approval.execute({
      kind: "semantic",
      name: "web_page_submit",
      input,
      element: { id: input.id, role: "form", name: accessibleName(form) },
      readOnly: false,
    }, () => {
      context?.signal?.throwIfAborted?.();
      if (typeof form.requestSubmit === "function") form.requestSubmit();
      else if (typeof form.submit === "function") form.submit();
      else throw new Error("page form cannot be submitted");
      return { submitted: input.id, url: document.location?.href ?? "" };
    });
  }

  function describeElement(element) {
    const tag = String(element.tagName ?? "").toLowerCase();
    const type = String(element.getAttribute?.("type") ?? element.type ?? "").toLowerCase();
    const value = type === "password" ? undefined
      : ("checked" in element && (type === "checkbox" || type === "radio"))
        ? Boolean(element.checked)
        : "value" in element ? String(element.value ?? "") : undefined;
    const form = element.form ? retain(element.form) : undefined;
    return deepFreeze({
      id: retain(element),
      role: element.getAttribute?.("role") || inferredRole(tag, type),
      name: accessibleName(element),
      tag,
      ...(type ? { type } : {}),
      ...(value === undefined ? {} : { value }),
      ...(element.href ? { href: String(element.href) } : {}),
      ...(form ? { form } : {}),
    });
  }

  function retain(element) {
    let id = ids.get(element);
    if (!id) {
      id = `page-${nextId++}`;
      ids.set(element, id);
    }
    elements.set(id, element);
    return id;
  }

  function requiredElement(id) {
    requireOpen();
    if (typeof id !== "string" || !id) throw new TypeError("page element ID must not be empty");
    let element = elements.get(id);
    if (!element) {
      observe();
      element = elements.get(id);
    }
    if (!element || !element.isConnected || !visible(element)) {
      throw new Error(`page element is no longer available: ${id}`);
    }
    return element;
  }

  function requireOpen() {
    if (closed) throw new Error("semantic page tools are closed");
  }
}

function generatedExecute(tool, document, options) {
  const custom = options.handlers?.[tool.name];
  let implementation;
  if (typeof custom === "function") implementation = custom;
  else if (tool.implementation?.kind === "fetch") {
    implementation = (input, context) => executeGeneratedFetch(tool, input, context, options);
  } else if (tool.implementation?.kind === "form") {
    implementation = (input) => executeGeneratedForm(tool, input, document);
  } else {
    throw new Error(`approved WebMCP tool requires a handler: ${tool.name}`);
  }
  return async (input, executeOptions = {}) => {
    executeOptions.signal?.throwIfAborted?.();
    if (tool.annotations?.readOnlyHint !== true) {
      await authorize(options.confirm, {
        kind: "published",
        name: tool.name,
        title: tool.title,
        input,
        readOnly: false,
      });
    }
    executeOptions.signal?.throwIfAborted?.();
    return implementation(input ?? {}, {
      signal: executeOptions.signal ?? new AbortController().signal,
    });
  };
}

async function executeGeneratedFetch(tool, input, context, options) {
  const implementation = tool.implementation;
  const fetchImpl = options.fetch ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") throw new Error("generated WebMCP fetch tool requires fetch");
  const baseUrl = options.baseUrl ?? options.document?.location?.href ?? globalThis.location?.href;
  const base = new URL(baseUrl);
  const url = new URL(fillPath(implementation.path, input.path), base);
  if (url.origin !== base.origin) {
    throw new Error(`generated WebMCP fetch must stay on ${base.origin}`);
  }
  if (input.query && typeof input.query === "object" && !Array.isArray(input.query)) {
    for (const [name, value] of Object.entries(input.query)) {
      if (value !== undefined && value !== null) url.searchParams.set(name, String(value));
    }
  }
  const method = String(implementation.method ?? "GET").toUpperCase();
  const response = await fetchImpl(url, {
    method,
    credentials: "same-origin",
    headers: input.body === undefined ? undefined : { "content-type": "application/json" },
    body: input.body === undefined || method === "GET" || method === "HEAD"
      ? undefined
      : JSON.stringify(input.body),
    signal: context.signal,
  });
  const text = await response.text();
  let body = text;
  try { body = text ? JSON.parse(text) : null; } catch {}
  if (!response.ok) {
    throw new Error(`${tool.name} failed with HTTP ${response.status}: ${text.slice(0, 2_000)}`);
  }
  return body;
}

function executeGeneratedForm(tool, input, document) {
  const selector = tool.implementation.selector;
  const form = document.querySelector?.(selector);
  if (!form || String(form.tagName ?? "").toLowerCase() !== "form") {
    throw new Error(`generated WebMCP form is unavailable: ${selector}`);
  }
  const fields = input.fields;
  if (!fields || typeof fields !== "object" || Array.isArray(fields)) {
    throw new TypeError(`${tool.name} requires an object named fields`);
  }
  for (const [name, value] of Object.entries(fields)) {
    const control = form.elements?.namedItem?.(name)
      ?? form.querySelector?.(`[name="${cssEscape(name)}"]`);
    if (!control) throw new Error(`form field is unavailable: ${name}`);
    setControlValue(control, value, document);
  }
  if (typeof form.requestSubmit === "function") form.requestSubmit();
  else if (typeof form.submit === "function") form.submit();
  else throw new Error("generated WebMCP form cannot be submitted");
  return { submitted: true };
}

function validateGeneratedTool(tool) {
  if (!tool || typeof tool !== "object"
      || typeof tool.name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(tool.name)
      || typeof tool.description !== "string" || !tool.description.trim()) {
    throw new TypeError("approved WebMCP manifest contains an invalid tool");
  }
  return tool;
}

function validateProviderOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("WebMCP provider options must be an object");
  }
  const allowed = new Set([
    "confirm", "dialog", "document", "fallback", "fromOrigins", "maxElements",
    "maxTextChars", "native", "sourceId",
  ]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported WebMCP provider option: ${name}`);
  }
  if (options.confirm !== undefined && typeof options.confirm !== "function") {
    throw new TypeError("WebMCP confirm must be a function");
  }
  if (options.dialog !== undefined && typeof options.dialog?.open !== "function") {
    throw new TypeError("WebMCP dialog must provide open(request, execution)");
  }
  if (options.native !== undefined
      && options.native !== true && options.native !== false && options.native !== "require") {
    throw new TypeError("WebMCP native must be true, false, or require");
  }
}

function validatePublishOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("WebMCP publish options must be an object");
  }
  const allowed = new Set([
    "baseUrl", "confirm", "document", "exposedTo", "fetch", "handlers",
  ]);
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) throw new TypeError(`unsupported WebMCP publish option: ${name}`);
  }
  if (options.confirm !== undefined && typeof options.confirm !== "function") {
    throw new TypeError("WebMCP publish confirm must be a function");
  }
}

function normalizeFallback(value = "when-empty") {
  if (value === true) return "when-empty";
  if (value === false) return "never";
  if (value === "always" || value === "when-empty" || value === "never") return value;
  throw new TypeError("WebMCP fallback must be always, when-empty, never, true, or false");
}

function normalizeOrigins(value) {
  if (value === undefined) return undefined;
  if (!Array.isArray(value) || value.some((origin) => typeof origin !== "string" || !origin)) {
    throw new TypeError("WebMCP origins must be an array of non-empty strings");
  }
  return Object.freeze([...new Set(value)]);
}

function definition(name, description, parameters) {
  return deepFreeze({
    type: "function",
    name,
    description,
    strict: false,
    defer_loading: true,
    parameters,
  });
}

function jsonSchema(value) {
  if (value === undefined) return { type: "object", additionalProperties: true };
  try { return JSON.parse(JSON.stringify(value)); }
  catch (error) { throw new TypeError("WebMCP input schema must be JSON-serializable", { cause: error }); }
}

function mirroredName(name) {
  const normalized = [...name.trim()]
    .map((character) => /[A-Za-z0-9_-]/.test(character) ? character : "_")
    .join("")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "") || "tool";
  const full = `web_${normalized}`;
  return full.length <= 64 ? full : `${full.slice(0, 55)}_${shortHash(name)}`;
}

function shortHash(value) {
  let hash = 0x811c9dc5;
  for (const character of value) {
    hash ^= character.codePointAt(0);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36).padStart(7, "0").slice(0, 7);
}

function parseStructuredResult(value) {
  if (typeof value !== "string") return value;
  try { return JSON.parse(value); } catch { return value; }
}

async function authorize(confirm, request) {
  if (confirm === undefined) return;
  if (await confirm(Object.freeze({ ...request })) !== true) {
    throw new Error(`WebMCP action was not approved: ${request.name}`);
  }
}

function createApproval(document, options) {
  const confirm = options.confirm;
  let dialog = options.dialog;
  let defaultDialog;
  return Object.freeze({
    async execute(request, action) {
      const frozen = deepFreeze(cloneApprovalValue({ ...request }));
      if (confirm !== undefined) {
        await authorize(confirm, frozen);
        return action();
      }
      if (!dialog) {
        defaultDialog ??= import("../cloud/Dialog.mjs").then(({ iframe }) => iframe().setup({
          appId: approvalApp(document).id,
        }));
        dialog = await defaultDialog;
      }
      return dialog.open(deepFreeze({
        id: randomId(),
        type: "webMcpApproval",
        app: approvalApp(document),
        action: frozen,
      }), { execute: action });
    },
  });
}

function cloneApprovalValue(value) {
  try {
    if (typeof globalThis.structuredClone === "function") return globalThis.structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  } catch (error) {
    throw new TypeError("WebMCP approval data must be structured-cloneable", { cause: error });
  }
}

function approvalApp(document) {
  const href = document.location?.href ?? globalThis.location?.href;
  let origin = document.location?.origin ?? globalThis.location?.origin;
  if (!origin && href) {
    try { origin = new URL(href).origin; } catch {}
  }
  if (!origin || origin === "null") {
    throw new Error("mutating WebMCP tools require a trusted browser origin");
  }
  const host = new URL(origin).hostname;
  return deepFreeze({
    id: `webmcp:${host}`.slice(0, 128),
    name: nonEmptyString(document.title) ?? host,
    origin,
  });
}

function randomId() {
  if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  return `webmcp-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function queryAll(document, selector) {
  const value = document.querySelectorAll?.(selector);
  return value ? Array.from(value) : [];
}

function visible(element) {
  if (!element || element.hidden || element.disabled || element.getAttribute?.("aria-hidden") === "true") {
    return false;
  }
  if (typeof element.getClientRects === "function" && element.getClientRects().length === 0) {
    return false;
  }
  return true;
}

function accessibleName(element) {
  const labelled = element.getAttribute?.("aria-label")
    || element.getAttribute?.("title")
    || element.labels?.[0]?.innerText
    || element.innerText
    || element.getAttribute?.("placeholder")
    || element.getAttribute?.("name")
    || "";
  return String(labelled).replace(/\s+/g, " ").trim().slice(0, 512);
}

function inferredRole(tag, type) {
  if (tag === "a") return "link";
  if (tag === "button") return "button";
  if (tag === "select") return "combobox";
  if (tag === "textarea") return "textbox";
  if (tag === "input" && (type === "checkbox" || type === "radio")) return type;
  if (tag === "input") return "textbox";
  return "control";
}

function setControlValue(element, value, document) {
  const type = String(element.getAttribute?.("type") ?? element.type ?? "").toLowerCase();
  if (type === "checkbox" || type === "radio") {
    element.checked = Boolean(value);
  } else if ("value" in element) {
    element.value = value === null ? "" : String(value);
  } else if (element.isContentEditable) {
    element.textContent = value === null ? "" : String(value);
  } else {
    throw new TypeError("page element is not a fillable control");
  }
  const EventImpl = document.defaultView?.Event ?? globalThis.Event;
  if (typeof EventImpl === "function" && typeof element.dispatchEvent === "function") {
    element.dispatchEvent(new EventImpl("input", { bubbles: true }));
    element.dispatchEvent(new EventImpl("change", { bubbles: true }));
  }
}

function fillPath(template, values = {}) {
  if (typeof template !== "string" || !template) throw new TypeError("generated fetch path is invalid");
  return template.replace(/\[([^\]]+)\]|:([A-Za-z0-9_]+)/g, (match, bracket, colon) => {
    const name = bracket ?? colon;
    const value = values?.[name];
    if (value === undefined || value === null) throw new TypeError(`missing path parameter: ${name}`);
    return encodeURIComponent(String(value));
  });
}

function cssEscape(value) {
  if (globalThis.CSS?.escape) return globalThis.CSS.escape(value);
  return String(value).replace(/["\\]/g, "\\$&");
}

function boundedInteger(value, name, minimum, maximum) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new TypeError(`${name} must be an integer from ${minimum} through ${maximum}`);
  }
  return value;
}

function nonEmptyString(value) {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}

function reportError(error) {
  if (typeof globalThis.reportError === "function") globalThis.reportError(error);
  else queueMicrotask(() => { throw error; });
}
