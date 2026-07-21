(() => {
  const nativeTool = __nanocodexTool;
  const nativeNotify = __nanocodexNotify;
  const nativeYield = __nanocodexYield;
  const nativeSetTimeout = __nanocodexSetTimeout;
  const nativeClearTimeout = __nanocodexClearTimeout;
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const imageDetails = new Set(["auto", "low", "high", "original"]);
  const imageHelperExpects =
    "image expects a non-empty image URL string or an object with image_url and optional detail";
  const invalidImageOutput =
    "Tool call failed: invalid image output. Pass a base64 data URI instead";
  const remoteImageOutput =
    "Tool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead";
  const jsonParse = JSON.parse;
  const jsonStringify = JSON.stringify;
  const cloneValue = typeof globalThis.structuredClone === "function"
    ? globalThis.structuredClone
    : (value) => value === undefined
      ? undefined
      : jsonParse(jsonStringify(value));

  function stringify(value) {
    if (typeof value === "string") return value;
    if (value === undefined) return "undefined";
    try { return jsonStringify(value); } catch { return String(value); }
  }

  function errorText(error) {
    if (!error) return String(error);
    const message = error.message;
    const label = message && error.name ? `${error.name}: ${message}` : message;
    if (error.stack && label && !error.stack.startsWith(label)) {
      return `${label}\n${error.stack}`;
    }
    return error.stack || label || String(error);
  }

  function storageKey(value, helper) {
    try {
      return `${value}`;
    } catch {
      throw `${helper} key must be a string`;
    }
  }

  function storedValue(key, value) {
    let encoded;
    try {
      encoded = jsonStringify(value);
    } catch (error) {
      throw errorText(error);
    }
    if (encoded === undefined) {
      throw `Unable to store ${jsonStringify(key)}. Only plain serializable objects can be stored.`;
    }
    return jsonParse(encoded);
  }

  return async function runCell(source, definitionsJson, initialStoredJson) {
    const definitions = jsonParse(definitionsJson);
    const initialStored = jsonParse(initialStoredJson);
    const content = [];
    const stored = new Map(Object.entries(initialStored));
    const storedWrites = new Map();
    let pendingToolCalls = 0;
    const declaredTools = Object.create(null);
    const invokeTool = (name, input) => {
      pendingToolCalls += 1;
      const encodedInput = input === undefined ? "null" : jsonStringify(input);
      return nativeTool(name, encodedInput)
        .then(jsonParse, (payload) => Promise.reject(jsonParse(payload)))
        .finally(() => { pendingToolCalls -= 1; });
    };
    for (const definition of definitions) {
      declaredTools[definition.name] = (input) => invokeTool(definition.name, input);
    }
    const tools = new Proxy(declaredTools, {
      get(target, property) {
        if (typeof property !== "string") return Reflect.get(target, property);
        return target[property] || ((input) => invokeTool(property, input));
      },
    });
    Object.freeze(tools);

    function text(value) {
      content.push({ type: "input_text", text: stringify(value) });
    }

    function notify(value) {
      const notification = stringify(value);
      if (!notification.trim()) throw "notify expects non-empty text";
      nativeNotify(notification);
    }

    function image(value, detail) {
      let imageUrl;
      let embeddedDetail;
      if (typeof value === "string") {
        imageUrl = value;
      } else if (
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        typeof value.image_url === "string"
      ) {
        imageUrl = value.image_url;
        embeddedDetail = value.detail;
      } else {
        throw imageHelperExpects;
      }

      if (!imageUrl) throw imageHelperExpects;
      const separator = imageUrl.indexOf(":");
      if (separator < 0) throw invalidImageOutput;
      const scheme = imageUrl.slice(0, separator).toLowerCase();
      if (scheme === "http" || scheme === "https") throw remoteImageOutput;
      if (scheme !== "data") throw invalidImageOutput;

      const selectedDetail = detail != null
        ? detail
        : embeddedDetail != null
          ? embeddedDetail
          : "high";
      if (typeof selectedDetail !== "string") {
        throw "image detail must be one of: auto, low, high, original";
      }
      const normalizedDetail = selectedDetail.toLowerCase();
      if (!imageDetails.has(normalizedDetail)) {
        throw "image detail must be one of: auto, low, high, original";
      }
      content.push({
        type: "input_image",
        image_url: imageUrl,
        detail: normalizedDetail,
      });
    }

    function generatedImage(value) {
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw "generatedImage expects an image generation result object";
      }
      const outputHint = value.output_hint;
      if (outputHint !== undefined && typeof outputHint !== "string") {
        throw "generatedImage output_hint must be a string when provided";
      }
      image(value);
      if (outputHint !== undefined) {
        content.push({ type: "input_text", text: outputHint });
      }
    }

    function store(key, value) {
      const normalizedKey = storageKey(key, "store");
      const normalizedValue = storedValue(normalizedKey, value);
      stored.set(normalizedKey, normalizedValue);
      storedWrites.set(normalizedKey, normalizedValue);
    }

    function load(key) {
      const normalizedKey = storageKey(key, "load");
      return stored.has(normalizedKey) ? cloneValue(stored.get(normalizedKey)) : undefined;
    }

    async function yield_control() {
      if (pendingToolCalls) {
        throw new Error("yield_control() cannot run while nested tool calls are pending");
      }
      nativeYield(jsonStringify(content.splice(0)));
      await Promise.resolve();
    }

    const EXIT = Symbol("exit");
    function exit() { throw EXIT; }

    const allTools = Object.freeze(definitions.map((tool) => Object.freeze({ ...tool })));
    try {
      const script = new AsyncFunction(
        "tools",
        "ALL_TOOLS",
        "text",
        "image",
        "generatedImage",
        "notify",
        "store",
        "load",
        "yield_control",
        "exit",
        "setTimeout",
        "clearTimeout",
        source,
      );
      try {
        await script(
          tools,
          allTools,
          text,
          image,
          generatedImage,
          notify,
          store,
          load,
          yield_control,
          exit,
          nativeSetTimeout,
          nativeClearTimeout,
        );
      } catch (error) {
        if (error !== EXIT) throw error;
      }
      return jsonStringify({
        type: "done",
        content,
        stored: Object.fromEntries(storedWrites),
      });
    } catch (error) {
      return jsonStringify({
        type: "error",
        message: errorText(error),
        content,
        stored: Object.fromEntries(storedWrites),
      });
    }
  };
})()
