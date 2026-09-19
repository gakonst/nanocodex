export const CUA_JS_NAME = "mcp__cua_repl__js";
export const CUA_RESET_NAME = "mcp__cua_repl__js_reset";
export const CUA_DESCRIPTION = "Control native apps or browsers on the user’s computer by reading or operating UI. Prefer purpose-built skills, connectors, APIs, or CLIs when available.\n\nOn your first call, or after a reset, execute exactly one of the API calls shown below, optionally assigning its result to a variable. Do not add other API calls, waits, or snapshots to that invocation.\nThe tool result will include documentation and, when creating or selecting a tab or selecting an app, its initial UI state. Selecting a browser does not open a tab. Read that result before continuing.\nUse only APIs described in the tool instructions or returned documentation.\n\nWhen you need an inventory of available apps, browsers, and tabs, get a snapshot of all enabled surfaces. Otherwise, use the relevant entry point below:\n\n```javascript\nawait cua.getState();\n```\n\nUse the first matching browser control option from the user's request:\n\nFor a tab @-mention (`mention=tab-v1`):\nPass the complete `plugin://...` URL to get the referenced tab.\n\n```javascript\nlet tab = await cua.getTab({ mention: tabMentionUrl });\n```\n\nFor an existing tab identified by URL in browser context (including the current IAB tab):\n\n```javascript\nlet tab = await cua.getTab({ url }, { browser: browserId });\n```\n\nKnown tab ID (`tabId` or `providerTabId`) and browser (name or browser @-mention):\n\n```javascript\nlet tab = await cua.getTab(tabId, { browser: browserId });\n```\n\nTo open a URL in the in-app browser (`@Browser`):\n\n```javascript\nlet tab = await cua.createBrowserTab(\"iab\", url, { visible: boolean });\n```\n\nTo open a URL in another named browser: pass its name directly; do not call `getBrowser` first.\n\n```javascript\nlet tab = await cua.createBrowserTab(browserName, url, browserOptions);\n```\n\nKnown URL, only when the user has not specified a browser by name or @-mention:\n\n```javascript\nlet browser = await cua.getBrowser({ url });\n```\n\nBrowser IDs and options:\n\n- `\"iab\"` (in-app browser): in `createBrowserTab`, use `visible: true` to show the browser; `false` to keep it hidden.\n- `\"chrome\"` (@Chrome), `\"edge\"` (@Edge): pass a short, emoji-prefixed `sessionName` (e.g. `\"🔎 Task\"`) to `createBrowserTab` when starting a task.\n\nIf the user specifies an app to use, get the app by name, bundle ID, or path:\n\n```javascript\nlet app = await cua.getApp(\"Example App\");\n```\n\nTo add other content to the tool result, use `nodeRepl.write(value)` for text or other values and `await nodeRepl.emitImage(image)` for images. The APIs listed above already display their documentation or UI state; do not wrap their results in `write` or `emitImage`.\n\nIf your context begins with a summary of an existing computer use task, call `await cua.rewriteDocumentation()` before continuing the computer use task to ensure you have a complete view of the necessary documentation.";
export const CUA_PARAMETERS = Object.freeze({
  "additionalProperties": false,
  "properties": {
    "code": {
      "description": "JavaScript to execute using the initialized cua_repl runtime.",
      "type": "string"
    },
    "timeout_ms": {
      "description": "Optional execution timeout in milliseconds. Defaults to 30000 (30 seconds) when omitted.",
      "minimum": 1,
      "type": "integer"
    },
    "title": {
      "description": "Short user-facing description of what the code does.",
      "maxLength": 80,
      "minLength": 1,
      "type": "string"
    }
  },
  "required": [
    "code"
  ],
  "type": "object"
});
export const CUA_RESET_DESCRIPTION = "Reset the persistent cua_repl JavaScript session. All JavaScript bindings are discarded. The next cua_repl.js call initializes a fresh runtime for the enabled surfaces. This does not close browser tabs or native apps, or erase their state.";
export const CUA_RESET_PARAMETERS = Object.freeze({
  "additionalProperties": false,
  "properties": {},
  "type": "object"
});

export function validateInput(input, reset = false) {
  if (reset && input == null) return {};
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("CUA expects an object");
  const allowed = reset ? [] : ["code", "title", "timeout_ms"];
  if (Object.keys(input).some(key => !allowed.includes(key))) throw new TypeError("Unknown CUA argument");
  if (reset) return {};
  if (typeof input.code !== "string") throw new TypeError("CUA code must be a string");
  if (input.title != null && (typeof input.title !== "string" || !input.title.trim())) throw new TypeError("CUA title must be non-empty");
  const value = { ...input };
  if (value.timeout_ms == null) delete value.timeout_ms;
  else if (!Number.isSafeInteger(value.timeout_ms) || value.timeout_ms < 1) throw new RangeError("CUA timeout must be a positive safe integer in milliseconds");
  return value;
}
