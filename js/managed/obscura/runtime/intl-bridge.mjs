/** Real native Intl RPC for a QuickJS realm. No locale tables or formatter stubs. */
const TYPES = new Set([
  "DateTimeFormat",
  "NumberFormat",
  "Collator",
  "PluralRules",
  "RelativeTimeFormat",
  "ListFormat",
  "DisplayNames",
  "Locale",
  "Segmenter",
]);
const CALLS = new Set([
  "resolvedOptions",
  "format",
  "formatToParts",
  "formatRange",
  "formatRangeToParts",
  "compare",
  "select",
  "selectRange",
  "of",
  "toString",
  "maximize",
  "minimize",
  "getCalendars",
  "getCollations",
  "getHourCycles",
  "getNumberingSystems",
  "getTextInfo",
  "getTimeZones",
  "getWeekInfo",
]);
const PROPS = new Set([
  "baseName",
  "calendar",
  "calendars",
  "caseFirst",
  "collation",
  "collations",
  "firstDayOfWeek",
  "hourCycle",
  "hourCycles",
  "language",
  "numberingSystem",
  "numberingSystems",
  "numeric",
  "region",
  "script",
  "timeZones",
  "weekInfo",
  "textInfo",
]);
const revive = (value) => {
  if (!value || typeof value !== "object") return value;
  if (value.$intl === "undefined") return undefined;
  if (value.$intl === "bigint") return BigInt(value.value);
  if (value.$intl === "number") return Number(value.value);
  if (Array.isArray(value)) return value.map(revive);
  return Object.fromEntries(
    Object.entries(value).map(([key, val]) => [key, revive(val)]),
  );
};
/** Attach before evaluating intl-installer.js; returns disposal for bounded cache/bridge state. */
export function attachIntlBridge(vm, NativeIntl = globalThis.Intl) {
  if (!NativeIntl) throw new Error("Host has no native Intl");
  let disposed = false;
  const cache = new Map();
  const bridge = vm.newFunction("__obscura_native_intl", (requestHandle) => {
    let response;
    try {
      if (disposed) throw new Error("Intl bridge was disposed");
      const wire = JSON.parse(vm.getString(requestHandle));
      const request = revive(wire),
        { action, type, method, args = [] } = request;
      let value;
      if (action === "builtin") {
        if (
          type === "Date" &&
          [
            "toLocaleString",
            "toLocaleDateString",
            "toLocaleTimeString",
          ].includes(method)
        )
          value = Date.prototype[method].apply(new Date(request.value), args);
        else if (type === "Number" && method === "toLocaleString")
          value = Number.prototype.toLocaleString.apply(request.value, args);
        else if (type === "BigInt" && method === "toLocaleString")
          value = BigInt.prototype.toLocaleString.apply(request.value, args);
        else if (type === "String" && method === "localeCompare")
          value = String.prototype.localeCompare.apply(request.value, args);
        else
          throw new Error("Unsupported Intl builtin: " + type + "." + method);
      } else if (action === "static") {
        if (type === "Intl") {
          if (!["getCanonicalLocales", "supportedValuesOf"].includes(method))
            throw new Error("Unsupported Intl static: " + method);
          if (typeof NativeIntl[method] !== "function")
            throw new Error("Host Intl does not support " + method);
          value = NativeIntl[method](...args);
        } else {
          if (
            !TYPES.has(type) ||
            typeof NativeIntl[type] !== "function" ||
            method !== "supportedLocalesOf"
          )
            throw new Error(
              "Unsupported Intl constructor/static: " + type + "." + method,
            );
          value = NativeIntl[type].supportedLocalesOf(...args);
        }
      } else {
        if (!TYPES.has(type) || typeof NativeIntl[type] !== "function")
          throw new Error("Unsupported host Intl constructor: " + type);
        const key = JSON.stringify([wire.type, wire.locales, wire.options]);
        let instance = cache.get(key);
        if (!instance) {
          instance = new NativeIntl[type](request.locales, request.options);
          if (cache.size >= 256) cache.delete(cache.keys().next().value);
          cache.set(key, instance);
        }
        if (action === "construct") value = true;
        else if (action === "get") {
          if (type !== "Locale" || !PROPS.has(method))
            throw new Error(
              "Unsupported Intl property: " + type + "." + method,
            );
          if (!(method in instance))
            throw new Error(
              "Host Intl does not support " + type + "." + method,
            );
          value = instance[method];
        } else if (action === "segment") {
          if (type !== "Segmenter")
            throw new Error("Unsupported segment receiver");
          const segments = instance.segment(args[0]);
          value =
            request.containing === undefined
              ? Array.from(segments)
              : segments.containing(request.containing);
        } else if (action === "call") {
          if (!CALLS.has(method) || typeof instance[method] !== "function")
            throw new Error(
              "Host Intl does not support " + type + "." + method,
            );
          value = instance[method](...args);
          if (method === "maximize" || method === "minimize")
            value = value.toString();
        } else throw new Error("Unsupported Intl bridge action: " + action);
      }
      response = { ok: true, value };
    } catch (error) {
      response = {
        ok: false,
        name: error.name || "Error",
        message: String(error.message || error),
      };
    }
    return vm.newString(JSON.stringify(response));
  });
  vm.setProp(vm.global, "__obscura_native_intl", bridge);
  bridge.dispose();
  return {
    dispose() {
      disposed = true;
      cache.clear();
    },
    get cacheSize() {
      return cache.size;
    },
  };
}
