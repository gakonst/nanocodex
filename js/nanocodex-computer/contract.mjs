export const CUA_JS_NAME = "mcp__cua_repl__js";
export const CUA_RESET_NAME = "mcp__cua_repl__js_reset";
export const CUA_DESCRIPTION = "Control native apps or browsers on the user’s computer by reading or operating UI. Prefer purpose-built skills, connectors, APIs, or CLIs when available.\n\nOn your first call, or after a reset, execute exactly one of the API calls shown below, optionally assigning its result to a variable. Do not add other API calls, waits, or snapshots to that invocation.\nThe tool result will include documentation and, when creating or selecting a tab or selecting an app, its initial UI state. Selecting a browser does not open a tab. Read that result before continuing.\nUse only APIs described in the tool instructions or returned documentation.\n\nWhen you need an inventory of available apps, browsers, and tabs, get a snapshot of all enabled surfaces. Otherwise, use the relevant entry point below:\n\n```javascript\nawait cua.getState();\n```\n\n\nTo see the Mac screen, capture the main display directly. This read-only screenshot includes the desktop and visible windows, works when Finder has no open window, and does not launch or activate an app:\n\n```javascript\nawait cua.getScreenshot();\n```\n\nUse `cua.getApp(...)` for a specific app's window. Desktop screenshot pixels are not app-relative coordinates for app actions.\n\nUse the first matching browser control option from the user's request:\n\nFor a tab @-mention (`mention=tab-v1`):\nCall `cua.getState()` and find the tab whose `providerTabId`/`title`/`url` all match the mention’s decoded `tabId`/`title`/`url`. Then call `cua.getTab(tabId, { browser: browserId })`, using the id fields from that tab and its browser.\n\nKnown tab ID (`tabId` or `providerTabId`) and browser (name or browser @-mention):\n```javascript\nlet tab = await cua.getTab(tabId, { browser: browserId });\n```\n\nKnown URL and in-app browser (`@Browser`):\n```javascript\nlet tab = await cua.createBrowserTab(\"iab\", url, { visible: boolean });\n```\n\nKnown URL and other named browser: pass its name directly; do not call `getBrowser` first.\n```javascript\nlet tab = await cua.createBrowserTab(browserName, url, browserOptions);\n```\n\nKnown URL, only when the user has not specified a browser by name or @-mention:\n```javascript\nlet browser = await cua.getBrowser({ url });\n```\n\nBrowser IDs and options:\n- `\"iab\"` (in-app browser): in `createBrowserTab`, use `visible: true` to show the browser; `false` to keep it hidden.\n- `\"chrome\"` (@Chrome), `\"edge\"` (@Edge): pass a short, emoji-prefixed `sessionName` (e.g. `\"🔎 Task\"`) to `createBrowserTab` when starting a task.\n\n\nIf the user specifies an app to use, get the app by name, bundle ID, or path:\n\n```javascript\nlet app = await cua.getApp(\"Example App\");\n```\n\nAfter initialization, when `cua.listWindows` is available, use `await cua.listWindows(\"Example App\")` to discover native windows. Bind an observed window with `await cua.getApp(\"Example App\", { windowId })` before controlling multiple windows. Keep a separate handle for each window. Background input preserves the human pointer; input to windows in the same Mac process is coordinated because they share keyboard focus.\n\n\nTo add other content to the tool result, use `nodeRepl.write(value)` for text or other values and `await nodeRepl.emitImage(image)` for images. The APIs listed above already display their documentation or UI state; do not wrap their results in `write` or `emitImage`.\n";
export const CUA_PARAMETERS = Object.freeze({
  type: "object",
  properties: {
    code: { type: "string", description: "JavaScript to execute using the initialized CUA runtime." },
    title: { type: "string", description: "Short user-facing description of what the code does.", minLength: 1 },
    timeout_ms: { type: "integer", description: "Optional caller-selected execution timeout in milliseconds. Omitted calls have no artificial deadline.", minimum: 1 },
  },
  required: ["code"], additionalProperties: false,
});
export const CUA_RESET_DESCRIPTION = "Reset the persistent CUA JavaScript session. All JavaScript bindings are discarded. The next cua_repl.js call initializes a fresh runtime for the enabled surfaces. This does not close browser tabs or native apps, or erase their state.\n";
export const CUA_RESET_PARAMETERS = Object.freeze({ type: "object", properties: {}, additionalProperties: false });

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
