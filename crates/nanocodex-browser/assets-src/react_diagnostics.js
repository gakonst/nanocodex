import {
  getDisplayName,
  getFiberFromHostInstance,
  instrument as instrumentFibers,
  isCompositeFiber,
} from "bippy";
import { getOwnerStack, getSource } from "bippy/source";
import { instrument as instrumentReact } from "react-scan/lite";

const INSTALL_KEY = "__nanocodexInstallReactDiagnostics";
const STATE_KEY = "__nanocodexReactDiagnostics";
const PROTOCOL_VERSION = 1;
const MAX_SNIPPET_CHARS = 4 * 1024;
const MAX_HTML_PREVIEW_CHARS = 4 * 1024;
const MAX_STYLES_CHARS = 8 * 1024;
const MAX_SELECTOR_CHARS = 2 * 1024;
const MAX_SOURCE_PATH_CHARS = 1024;
const MAX_SOURCE_TEXT_CHARS = 512;
const MAX_FUNCTION_NAME_CHARS = 256;
const MAX_OWNER_STACK_FRAMES = 32;
const MAX_OWNER_STACK_TEXT_CHARS = 4 * 1024;
const MAX_INDEXED_FIBERS_PER_COMMIT = 200 * 1000;
const SOURCE_RESOLUTION_TIMEOUT_MS = 10 * 1000;
const FIBER_HYDRATION_TIMEOUT_MS = 2 * 1000;
const PREFERRED_SELECTOR_ATTRIBUTES = [
  "data-testid",
  "data-test-id",
  "data-test",
  "data-cy",
  "data-qa",
  "aria-label",
  "role",
  "name",
  "title",
  "alt",
  "href",
  "src",
];
const RELEVANT_STYLE_PROPERTIES = [
  "display",
  "position",
  "inset",
  "top",
  "right",
  "bottom",
  "left",
  "z-index",
  "box-sizing",
  "width",
  "min-width",
  "max-width",
  "height",
  "min-height",
  "max-height",
  "margin",
  "padding",
  "overflow",
  "overflow-x",
  "overflow-y",
  "opacity",
  "visibility",
  "pointer-events",
  "flex",
  "flex-basis",
  "flex-direction",
  "flex-grow",
  "flex-shrink",
  "flex-wrap",
  "align-content",
  "align-items",
  "align-self",
  "justify-content",
  "justify-items",
  "justify-self",
  "gap",
  "row-gap",
  "column-gap",
  "grid",
  "grid-template-columns",
  "grid-template-rows",
  "grid-column",
  "grid-row",
  "color",
  "background",
  "background-color",
  "border",
  "border-radius",
  "box-shadow",
  "font",
  "font-family",
  "font-size",
  "font-style",
  "font-weight",
  "letter-spacing",
  "line-height",
  "text-align",
  "text-decoration",
  "text-overflow",
  "text-transform",
  "white-space",
  "word-break",
  "transform",
  "transform-origin",
  "transition",
  "animation",
  "object-fit",
  "object-position",
];

const fiberByElement = new WeakMap();

const integerOrNull = (value) =>
  Number.isSafeInteger(value) ? value : null;

const numberOrNull = (value) =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

const stringOrNull = (value) =>
  typeof value === "string" ? value : null;

const boundedString = (value, maximum) =>
  String(value ?? "").slice(0, maximum);

const boundedStringOrNull = (value, maximum) =>
  typeof value === "string" ? value.slice(0, maximum) : null;

const unsignedIntegerOrNull = (value) =>
  Number.isSafeInteger(value) && value >= 0 && value <= 0xffff_ffff
    ? value
    : null;

const normalizeSource = (source) => {
  if (source === null || typeof source !== "object") return null;
  return {
    fileName: String(source.fileName ?? ""),
    lineNumber: integerOrNull(source.lineNumber),
    columnNumber: integerOrNull(source.columnNumber),
    functionName: stringOrNull(source.functionName),
  };
};

const normalizeChangeDescription = (change) => {
  if (change === null || typeof change !== "object") return null;
  return {
    isFirstMount: change.isFirstMount === true,
    props: Array.isArray(change.props)
      ? change.props.map(String)
      : null,
    state: change.state === true,
    context: change.context === true,
    hooks: Array.isArray(change.hooks)
      ? change.hooks.filter(Number.isSafeInteger)
      : [],
    parent: change.parent === true,
  };
};

const normalizeFiber = (fiber) => ({
  name: String(fiber.name ?? ""),
  depth: integerOrNull(fiber.depth) ?? 0,
  tag: integerOrNull(fiber.tag) ?? -1,
  actualDurationMs: numberOrNull(fiber.actualDuration) ?? 0,
  actualStartTimeMs: numberOrNull(fiber.actualStartTime) ?? 0,
  selfBaseDurationMs: numberOrNull(fiber.selfBaseDuration) ?? 0,
  treeBaseDurationMs: numberOrNull(fiber.treeBaseDuration) ?? 0,
  fiberId: integerOrNull(fiber.fiberId),
  source: normalizeSource(fiber.source),
  ownerName: stringOrNull(fiber.ownerName),
  changeDescription: normalizeChangeDescription(fiber.changeDescription),
});

const normalizeOwnerFrame = (frame) => ({
  fileName: boundedStringOrNull(frame.fileName, MAX_SOURCE_PATH_CHARS),
  lineNumber: unsignedIntegerOrNull(frame.lineNumber),
  columnNumber: unsignedIntegerOrNull(frame.columnNumber),
  functionName: boundedStringOrNull(
    frame.functionName,
    MAX_FUNCTION_NAME_CHARS,
  ),
  source: boundedStringOrNull(frame.source, MAX_SOURCE_TEXT_CHARS),
  isServer: frame.isServer === true,
  isSymbolicated: frame.isSymbolicated === true,
  isIgnoreListed: frame.isIgnoreListed === true,
});

const isElement = (value) =>
  value !== null &&
  typeof value === "object" &&
  value.nodeType === Node.ELEMENT_NODE;

const indexFiberTree = (rootFiber) => {
  const pending = [rootFiber];
  let visited = 0;
  while (pending.length > 0 && visited < MAX_INDEXED_FIBERS_PER_COMMIT) {
    const fiber = pending.pop();
    if (fiber === null || typeof fiber !== "object") continue;
    visited++;
    if (isElement(fiber.stateNode)) {
      fiberByElement.set(fiber.stateNode, fiber);
    }
    if (fiber.sibling !== null) pending.push(fiber.sibling);
    if (fiber.child !== null) pending.push(fiber.child);
  }
};

const forgetFiber = (fiber) => {
  if (!isElement(fiber?.stateNode)) return;
  fiberByElement.delete(fiber.stateNode);
};

const composedParentElement = (element) => {
  if (element.parentElement !== null) return element.parentElement;
  const root = element.getRootNode();
  return root instanceof ShadowRoot ? root.host : null;
};

const findFiberContext = (element) => {
  let current = element;
  while (current !== null) {
    const fiber =
      fiberByElement.get(current) ?? getFiberFromHostInstance(current);
    if (fiber !== null && fiber !== undefined) {
      return { element: current, fiber };
    }
    current = composedParentElement(current);
  }
  return null;
};

const findEquivalentFiberContext = (element) => {
  const direct = findFiberContext(element);
  if (direct !== null) return direct;

  const root = element.isConnected
    ? selectorRoot(element)
    : element.ownerDocument;
  const attributes = ["id", ...PREFERRED_SELECTOR_ATTRIBUTES];
  for (const name of attributes) {
    const value = element.getAttribute(name);
    if (value === null || value.length === 0 || value.length > 256) continue;
    let candidates;
    try {
      candidates = root.querySelectorAll(
        `[${name}=${JSON.stringify(value)}]`,
      );
    } catch {
      continue;
    }
    for (const candidate of candidates) {
      const context = findFiberContext(candidate);
      if (context !== null) return context;
    }
  }
  return null;
};

const waitForFiberContext = async (element) => {
  let context = findEquivalentFiberContext(element);
  const hook = globalThis.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (
    context !== null ||
    !(hook?.renderers instanceof Map) ||
    hook.renderers.size === 0
  ) {
    return context;
  }

  const deadline = performance.now() + FIBER_HYDRATION_TIMEOUT_MS;
  while (context === null && performance.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 25));
    context = findEquivalentFiberContext(element);
  }
  return context;
};

const findComponentName = (fiber) => {
  let current = fiber;
  while (current !== null && current !== undefined) {
    if (isCompositeFiber(current)) {
      const displayName = getDisplayName(current.type);
      if (typeof displayName === "string" && displayName.length > 0) {
        return displayName;
      }
    }
    current = current.return;
  }
  return null;
};

const readFiberSource = async (fiber) => {
  const abort = new AbortController();
  const fetchSource = (url) =>
    fetch(url, { signal: abort.signal, priority: "high" });
  const read = Promise.all([
    getOwnerStack(fiber, true, fetchSource).catch(() => []),
    getSource(fiber, true, fetchSource).catch(() => null),
  ]).then(([stack, source]) => ({
    stack: Array.isArray(stack) ? stack : [],
    source:
      source !== null && typeof source === "object" ? source : null,
  }));
  let timeoutId;
  const timeout = new Promise((resolve) => {
    timeoutId = setTimeout(
      () => resolve({ stack: [], source: null }),
      SOURCE_RESOLUTION_TIMEOUT_MS,
    );
  });
  const result = await Promise.race([read, timeout]);
  clearTimeout(timeoutId);
  abort.abort();
  return result;
};

const selectorRoot = (element) => {
  const root = element.getRootNode();
  return root instanceof ShadowRoot ? root : element.ownerDocument;
};

const isUniqueSelector = (element, selector) => {
  try {
    const matches = selectorRoot(element).querySelectorAll(selector);
    return matches.length === 1 && matches[0] === element;
  } catch {
    return false;
  }
};

const localSelector = (element) => {
  const id = element.getAttribute("id");
  if (id !== null && id.length > 0) {
    const selector = `#${CSS.escape(id)}`;
    if (isUniqueSelector(element, selector)) return selector;
  }

  for (const name of PREFERRED_SELECTOR_ATTRIBUTES) {
    const value = element.getAttribute(name);
    if (value === null || value.length === 0 || value.length > 256) continue;
    const attribute = `[${name}=${JSON.stringify(value)}]`;
    if (isUniqueSelector(element, attribute)) return attribute;
    const selector = `${element.localName}${attribute}`;
    if (isUniqueSelector(element, selector)) return selector;
  }

  const segments = [];
  const root = element.getRootNode();
  let current = element;
  while (isElement(current)) {
    const currentId = current.getAttribute("id");
    if (currentId !== null && currentId.length > 0) {
      segments.unshift(`#${CSS.escape(currentId)}`);
      break;
    }
    const parent = current.parentElement;
    if (parent === null) {
      segments.unshift(current.localName);
      break;
    }
    const siblings = Array.from(parent.children).filter(
      (sibling) => sibling.localName === current.localName,
    );
    const index = siblings.indexOf(current) + 1;
    segments.unshift(
      siblings.length === 1
        ? current.localName
        : `${current.localName}:nth-of-type(${index})`,
    );
    if (parent === root || parent === element.ownerDocument.body) break;
    current = parent;
  }
  return segments.join(" > ");
};

const stableSelector = (element) => {
  const local = localSelector(element);
  const root = element.getRootNode();
  return root instanceof ShadowRoot
    ? `${stableSelector(root.host)} >>> ${local}`
    : local;
};

const extractStyles = (element) => {
  const className = element.getAttribute("class")?.trim() ?? "";
  const declarations = [];
  let baselineFrame = null;
  try {
    baselineFrame = document.createElement("iframe");
    baselineFrame.style.cssText =
      "position:fixed;left:-9999px;width:0;height:0;border:0;visibility:hidden";
    document.body.appendChild(baselineFrame);
    const baselineDocument = baselineFrame.contentDocument;
    const baselineWindow = baselineFrame.contentWindow;
    if (baselineDocument === null || baselineWindow === null) return "";
    const baseline = baselineDocument.createElement(element.localName);
    baselineDocument.body.appendChild(baseline);
    const computed = getComputedStyle(element);
    const defaults = baselineWindow.getComputedStyle(baseline);
    for (const property of RELEVANT_STYLE_PROPERTIES) {
      const value = computed.getPropertyValue(property);
      if (
        value.length > 0 &&
        value !== defaults.getPropertyValue(property)
      ) {
        declarations.push(`${property}: ${value};`);
      }
    }
  } catch {
    // A page policy can prevent the clean baseline iframe. Class names still
    // provide useful styling context below.
  } finally {
    baselineFrame?.remove();
  }
  const css = declarations.join("\n");
  if (className.length === 0) return css;
  if (css.length === 0) return `className: ${className}`;
  return `className: ${className}\n\n${css}`;
};

const formatOwnerStack = (stack) =>
  stack
    .map((frame) => {
      if (typeof frame.source === "string" && frame.source.length > 0) {
        return frame.source;
      }
      const location =
        typeof frame.fileName === "string"
          ? `${frame.fileName}${
              Number.isSafeInteger(frame.lineNumber)
                ? `:${frame.lineNumber}`
                : ""
            }${
              Number.isSafeInteger(frame.columnNumber)
                ? `:${frame.columnNumber}`
                : ""
            }`
          : "";
      const name =
        typeof frame.functionName === "string"
          ? frame.functionName
          : "<anonymous>";
      return location.length > 0 ? `at ${name} (${location})` : `at ${name}`;
    })
    .join("\n");

const readElementContext = async (element) => {
  const fiberContext = await waitForFiberContext(element);
  const selectedElement = fiberContext?.element ?? element;
  const componentName =
    fiberContext === null
      ? null
      : boundedStringOrNull(
          findComponentName(fiberContext.fiber),
          MAX_FUNCTION_NAME_CHARS,
        );
  const resolved =
    fiberContext === null
      ? { stack: [], source: null }
      : await readFiberSource(fiberContext.fiber);
  const stack = resolved.stack
    .slice(0, MAX_OWNER_STACK_FRAMES)
    .map(normalizeOwnerFrame);
  const fileName = boundedStringOrNull(
    resolved.source?.fileName,
    MAX_SOURCE_PATH_CHARS,
  );
  const htmlPreview = boundedString(
    selectedElement.outerHTML,
    MAX_HTML_PREVIEW_CHARS,
  );
  const selector = boundedStringOrNull(
    stableSelector(selectedElement),
    MAX_SELECTOR_CHARS,
  );
  const ownerStackText = boundedString(
    formatOwnerStack(resolved.stack),
    MAX_OWNER_STACK_TEXT_CHARS,
  );
  const snippet = boundedString(
    [
      htmlPreview,
      componentName === null ? null : `component: ${componentName}`,
      fileName === null
        ? null
        : `source: ${fileName}${
            Number.isSafeInteger(resolved.source?.lineNumber)
              ? `:${resolved.source.lineNumber}`
              : ""
          }`,
      selector === null ? null : `selector: ${selector}`,
    ]
      .filter((part) => part !== null)
      .join("\n"),
    MAX_SNIPPET_CHARS,
  );
  return {
    protocolVersion: PROTOCOL_VERSION,
    context: {
      snippet,
      htmlPreview,
      componentName,
      source:
        fileName === null
          ? null
          : {
              fileName,
              lineNumber: unsignedIntegerOrNull(
                resolved.source?.lineNumber,
              ),
              columnNumber: unsignedIntegerOrNull(
                resolved.source?.columnNumber,
              ),
              functionName:
                boundedStringOrNull(
                  resolved.source?.functionName,
                  MAX_FUNCTION_NAME_CHARS,
                ) ?? componentName,
            },
      ownerStack: stack,
      ownerStackText,
      selector,
      styles: boundedString(extractStyles(selectedElement), MAX_STYLES_CHARS),
    },
  };
};

const rendererKey = (version, bundleType) =>
  `${version ?? "unknown"}:${bundleType ?? "unknown"}`;

const install = (options = {}) => {
  const existing = globalThis[STATE_KEY];
  if (existing !== undefined) return existing;

  const maxEvents = Math.max(
    1,
    Math.min(4096, integerOrNull(options.maxEvents) ?? 512),
  );
  const renderers = new Map();
  const events = [];
  let nextSequence = 1;
  let dropped = 0;
  let handle = null;
  let unsubscribe = null;

  const status = () => ({
    enabled: true,
    active: handle?.isActive() ?? true,
    rendererCount: renderers.size,
    renderers: Array.from(renderers.values()),
    documentUrl: String(globalThis.location?.href ?? ""),
    timeOriginMs: numberOrNull(globalThis.performance?.timeOrigin),
  });

  const recordRenderer = (event) => {
    const data =
      event.data !== null && typeof event.data === "object"
        ? event.data
        : {};
    const version =
      stringOrNull(event.reactVersion) ??
      stringOrNull(data.version) ??
      "unknown";
    const bundleType =
      integerOrNull(event.bundleType) ??
      integerOrNull(data.bundleType);
    const key = rendererKey(version, bundleType);
    const previous = renderers.get(key);
    renderers.set(key, {
      version,
      bundleType,
      profilingHooksAvailable:
        typeof event.available === "boolean"
          ? event.available
          : previous?.profilingHooksAvailable ?? null,
      profilingHooksUnavailableReason:
        stringOrNull(event.reason) ??
        previous?.profilingHooksUnavailableReason ??
        null,
      profilingHooksError:
        stringOrNull(event.error) ??
        previous?.profilingHooksError ??
        null,
    });
  };

  const onEvent = (event) => {
    if (
      event.kind === "renderer-injected" ||
      event.kind === "profiling-hooks-status"
    ) {
      recordRenderer(event);
    }
    const data =
      event.data !== null && typeof event.data === "object"
        ? event.data
        : {};
    const normalized = {
      sequence: nextSequence++,
      kind: String(event.kind),
      timestampMs: numberOrNull(event.timestamp) ?? 0,
      componentName: stringOrNull(event.componentName),
      lanes: integerOrNull(event.lanes),
      laneLabels: Array.isArray(event.laneLabels)
        ? event.laneLabels.map(String)
        : [],
      rendererId: integerOrNull(event.rendererId),
      priorityLevel: integerOrNull(event.priorityLevel),
      priorityName: stringOrNull(event.priorityName),
      didError: event.didError === true,
      tree: Array.isArray(event.tree)
        ? event.tree.map(normalizeFiber)
        : [],
      message: stringOrNull(event.message),
      rendererVersion:
        stringOrNull(event.reactVersion) ??
        stringOrNull(data.version),
      rendererBundleType:
        integerOrNull(event.bundleType) ??
        integerOrNull(data.bundleType),
      profilingHooksAvailable:
        typeof event.available === "boolean"
          ? event.available
          : null,
      profilingHooksUnavailableReason: stringOrNull(event.reason),
      profilingHooksError: stringOrNull(event.error),
    };
    if (events.length === maxEvents) {
      events.shift();
      dropped++;
    }
    events.push(normalized);
  };

  instrumentFibers({
    name: "nanocodex-element-context",
    onCommitFiberRoot(_rendererId, root) {
      indexFiberTree(root.current);
    },
    onCommitFiberUnmount(_rendererId, fiber) {
      forgetFiber(fiber);
    },
  });

  handle = instrumentReact({
    onEvent,
    includeFiberTree: true,
    includeProfilingHooks: options.includeProfilingHooks !== false,
    recordChangeDescriptions: true,
    includeFiberSource: true,
    includeFiberIdentity: true,
    includeLaneLabels: true,
    maxFibersPerCommit: 5000,
    minFiberActualDurationMs: 0,
  });
  unsubscribe = handle.subscribe(onEvent);

  const api = {
    protocolVersion: PROTOCOL_VERSION,
    status,
    read(request = {}) {
      const after = Math.max(0, integerOrNull(request.after) ?? 0);
      const limit = Math.max(
        1,
        Math.min(1000, integerOrNull(request.limit) ?? 200),
      );
      const matching = events.filter((event) => event.sequence > after);
      const page = matching.slice(0, limit);
      return {
        protocolVersion: PROTOCOL_VERSION,
        status: status(),
        events: page,
        total: events.length,
        dropped,
        lastSequence:
          page.length === 0 ? null : page[page.length - 1].sequence,
        hasMore: matching.length > page.length,
      };
    },
    elementContext(element) {
      return readElementContext(element);
    },
    stop() {
      unsubscribe?.();
      unsubscribe = null;
    },
  };
  Object.defineProperty(globalThis, STATE_KEY, {
    value: api,
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return api;
};

Object.defineProperty(globalThis, INSTALL_KEY, {
  value: install,
  configurable: false,
  enumerable: false,
  writable: false,
});
