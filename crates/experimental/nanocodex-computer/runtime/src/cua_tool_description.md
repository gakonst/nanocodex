Control native apps or browsers on the user’s computer by reading or operating UI. Prefer purpose-built skills, connectors, APIs, or CLIs when available.

On your first call, or after a reset, execute exactly one of the API calls shown below, optionally assigning its result to a variable. Do not add other API calls, waits, or snapshots to that invocation.
The tool result will include documentation and, when creating or selecting a tab or selecting an app, its initial UI state. Selecting a browser does not open a tab. Read that result before continuing.
Use only APIs described in the tool instructions or returned documentation.

When you need an inventory of available apps, browsers, and tabs, get a snapshot of all enabled surfaces. Otherwise, use the relevant entry point below:

```javascript
await cua.getState();
```


Use the first matching browser control option from the user's request:

For a tab @-mention (`mention=tab-v1`):
Call `cua.getState()` and find the tab whose `providerTabId`/`title`/`url` all match the mention’s decoded `tabId`/`title`/`url`. Then call `cua.getTab(tabId, { browser: browserId })`, using the id fields from that tab and its browser.

Known tab ID (`tabId` or `providerTabId`) and browser (name or browser @-mention):
```javascript
let tab = await cua.getTab(tabId, { browser: browserId });
```

Known URL and in-app browser (`@Browser`):
```javascript
let tab = await cua.createBrowserTab("iab", url, { visible: boolean });
```

Known URL and other named browser: pass its name directly; do not call `getBrowser` first.
```javascript
let tab = await cua.createBrowserTab(browserName, url, browserOptions);
```

Known URL, only when the user has not specified a browser by name or @-mention:
```javascript
let browser = await cua.getBrowser({ url });
```

Browser IDs and options:
- `"iab"` (in-app browser): in `createBrowserTab`, use `visible: true` to show the browser; `false` to keep it hidden.
- `"chrome"` (@Chrome), `"edge"` (@Edge): pass a short, emoji-prefixed `sessionName` (e.g. `"🔎 Task"`) to `createBrowserTab` when starting a task.


If the user specifies an app to use, get the app by name, bundle ID, or path:

```javascript
let app = await cua.getApp("Example App");
```


To add other content to the tool result, use `nodeRepl.write(value)` for text or other values and `await nodeRepl.emitImage(image)` for images. The APIs listed above already display their documentation or UI state; do not wrap their results in `write` or `emitImage`.
