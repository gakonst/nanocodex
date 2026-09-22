((createCodeTools, valueHelpers) => {
  const { stringify, storeSnapshot, normalizeImage, normalizeAudio, generatedImageItems } = valueHelpers;
  const nativeTool = __nanocodexTool;
  const nativeContent = __nanocodexContent;
  const nativeNotify = __nanocodexNotify;
  const nativeYield = __nanocodexYield;
  const nativeSetTimeout = __nanocodexSetTimeout;
  const nativeClearTimeout = __nanocodexClearTimeout;
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const jsonParse = JSON.parse;
  const jsonStringify = JSON.stringify;
  const cloneValue = typeof globalThis.structuredClone === "function"
    ? globalThis.structuredClone
    : (value) => value === undefined
      ? undefined
      : jsonParse(jsonStringify(value));

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
    const tools = createCodeTools(
      Object.keys(declaredTools),
      (name, input) => declaredTools[name](input),
    );

    function text(value) {
      nativeContent(jsonStringify({ type: "input_text", text: stringify(value) }));
    }

    function notify(value) {
      const notification = stringify(value);
      if (!notification.trim()) throw "notify expects non-empty text";
      nativeNotify(notification);
    }

    function image(value, detail) {
      nativeContent(jsonStringify(normalizeImage(value, detail)));
    }

    function audio(value) {
      nativeContent(jsonStringify(normalizeAudio(value)));
    }

    function generatedImage(value) {
      for (const item of generatedImageItems(value)) {
        nativeContent(jsonStringify(item));
      }
    }

    function store(key, value) {
      const normalizedKey = storageKey(key, "store");
      const [, normalizedValue] = storeSnapshot(normalizedKey, value);
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

    const allTools = Object.freeze(definitions.map((tool) => {
      return Object.freeze({
        name: tool.name,
        description: tool.description,
      });
    }));
    function toolSchema(name) {
      if (typeof name !== "string" || !name) {
        throw "toolSchema expects a non-empty tool name";
      }
      const definition = definitions.find((tool) =>
        tool.name === name || tool.tool_name === name
      );
      if (!definition) return undefined;
      return cloneValue({
        inputSchema: definition.input_schema ?? null,
        outputSchema: definition.output_schema ?? null,
      });
    }
    try {
      const script = new AsyncFunction(
        "tools",
        "ALL_TOOLS",
        "toolSchema",
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
          toolSchema,
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
})
