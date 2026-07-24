(() => {
  const nativeTool = __nanocodexTool;
  const nativeContent = __nanocodexContent;
  const nativeNotify = __nanocodexNotify;
  const nativeYield = __nanocodexYield;
  const nativeSetTimeout = __nanocodexSetTimeout;
  const nativeClearTimeout = __nanocodexClearTimeout;
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const imageDetails = new Set(["auto", "low", "high", "original"]);
  const imageHelperExpects =
    "image expects a non-empty image URL string, an object with image_url and optional detail, or a raw MCP image block";
  const audioHelperExpects =
    "audio expects a non-empty audio URL string, an object with audio_url, or a raw MCP audio block";
  const invalidImageOutput =
    "Tool call failed: invalid image output. Pass a base64 data URI instead";
  const remoteImageOutput =
    "Tool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead";
  const invalidAudioOutput =
    "Tool call failed: invalid audio output. Pass a base64 data URI instead";
  const jsonParse = JSON.parse;
  const jsonStringify = JSON.stringify;
  const cloneValue = typeof globalThis.structuredClone === "function"
    ? globalThis.structuredClone
    : (value) => value === undefined
      ? undefined
      : jsonParse(jsonStringify(value));

  function stringify(value) {
    if (
      value === undefined ||
      value === null ||
      typeof value === "boolean" ||
      typeof value === "number" ||
      typeof value === "bigint" ||
      typeof value === "string"
    ) {
      return String(value);
    }
    const encoded = jsonStringify(value);
    return encoded === undefined ? String(value) : encoded;
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
    const stored = new Map(Object.entries(initialStored));
    const storedWrites = new Map();
    const declaredTools = Object.create(null);
    const invokeTool = (name, input) => {
      const encodedInput = input === undefined ? "null" : jsonStringify(input);
      return nativeTool(name, encodedInput)
        .then(jsonParse, (payload) => Promise.reject(jsonParse(payload)));
    };
    for (const definition of definitions) {
      declaredTools[definition.name] = (input) => invokeTool(
        definition.tool_name,
        input === undefined && definition.kind === "function" ? {} : input,
      );
    }
    const tools = new Proxy(declaredTools, {
      get(target, property) {
        if (typeof property !== "string") return Reflect.get(target, property);
        return target[property] || ((input) => invokeTool(property, input));
      },
    });
    Object.freeze(tools);

    function text(value) {
      nativeContent(jsonStringify({ type: "input_text", text: stringify(value) }));
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
        Object.hasOwn(value, "image_url")
      ) {
        if (typeof value.image_url !== "string") throw imageHelperExpects;
        imageUrl = value.image_url;
        embeddedDetail = value.detail;
      } else if (value && typeof value === "object" && !Array.isArray(value)) {
        if (typeof value.type !== "string") throw imageHelperExpects;
        if (value.type !== "image") {
          throw `image only accepts MCP image blocks, got ${jsonStringify(value.type)}`;
        }
        if (typeof value.data !== "string" || !value.data) {
          throw "image expected MCP image data";
        }
        const mimeType =
          typeof value.mimeType === "string" && value.mimeType
            ? value.mimeType
            : typeof value.mime_type === "string" && value.mime_type
              ? value.mime_type
              : "application/octet-stream";
        imageUrl = value.data.toLowerCase().startsWith("data:")
          ? value.data
          : `data:${mimeType};base64,${value.data}`;
        const metadataDetail = value._meta?.["codex/imageDetail"];
        if (imageDetails.has(metadataDetail)) embeddedDetail = metadataDetail;
      } else {
        throw imageHelperExpects;
      }

      if (!imageUrl) throw imageHelperExpects;
      const separator = imageUrl.indexOf(":");
      if (separator < 0) throw invalidImageOutput;
      const scheme = imageUrl.slice(0, separator).toLowerCase();
      if (scheme === "http" || scheme === "https") throw remoteImageOutput;
      if (scheme !== "data") throw invalidImageOutput;

      if (detail !== undefined && detail !== null && typeof detail !== "string") {
        throw "image detail must be a string when provided";
      }
      if (
        embeddedDetail !== undefined &&
        embeddedDetail !== null &&
        typeof embeddedDetail !== "string"
      ) {
        throw "image detail must be a string when provided";
      }
      const selectedDetail = detail ?? embeddedDetail ?? "high";
      if (typeof selectedDetail !== "string") {
        throw "image detail must be one of: auto, low, high, original";
      }
      const normalizedDetail = selectedDetail.toLowerCase();
      if (!imageDetails.has(normalizedDetail)) {
        throw "image detail must be one of: auto, low, high, original";
      }
      nativeContent(jsonStringify({
        type: "input_image",
        image_url: imageUrl,
        detail: normalizedDetail,
      }));
    }

    function audio(value) {
      let audioUrl;
      if (typeof value === "string") {
        audioUrl = value;
      } else if (
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        Object.hasOwn(value, "audio_url")
      ) {
        if (typeof value.audio_url !== "string") throw audioHelperExpects;
        audioUrl = value.audio_url;
      } else if (value && typeof value === "object" && !Array.isArray(value)) {
        if (typeof value.type !== "string") throw audioHelperExpects;
        if (value.type !== "audio") {
          throw `audio only accepts MCP audio blocks, got ${jsonStringify(value.type)}`;
        }
        if (typeof value.data !== "string" || !value.data) {
          throw "audio expected MCP audio data";
        }
        const mimeType =
          typeof value.mimeType === "string" && value.mimeType
            ? value.mimeType
            : typeof value.mime_type === "string" && value.mime_type
              ? value.mime_type
              : "application/octet-stream";
        audioUrl = value.data.toLowerCase().startsWith("data:")
          ? value.data
          : `data:${mimeType};base64,${value.data}`;
      } else {
        throw audioHelperExpects;
      }
      if (!audioUrl) throw audioHelperExpects;
      const separator = audioUrl.indexOf(":");
      if (separator < 0 || audioUrl.slice(0, separator).toLowerCase() !== "data") {
        throw invalidAudioOutput;
      }
      nativeContent(jsonStringify({ type: "input_audio", audio_url: audioUrl }));
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
        text(outputHint);
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

    function yield_control() {
      nativeYield();
    }

    const EXIT = Symbol("exit");
    function exit() { throw EXIT; }

    const allTools = Object.freeze(definitions.map((tool) => Object.freeze({
      name: tool.name,
      description: tool.description,
    })));
    try {
      const script = new AsyncFunction(
        "tools",
        "ALL_TOOLS",
        "text",
        "image",
        "audio",
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
          audio,
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
        stored: Object.fromEntries(storedWrites),
      });
    } catch (error) {
      return jsonStringify({
        type: "error",
        message: errorText(error),
        stored: Object.fromEntries(storedWrites),
      });
    }
  };
})()
