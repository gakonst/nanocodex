# Nanocodex Chrome extension

Nanocodex for Chrome is a Manifest V3 consumer of the account-owned hosted
agent contract. The React side panel reuses the shared durable-agent terminal
and conversation rail, opens one exact durable agent per approved conversation,
and reverse-attaches one exact open Chrome tab only when the agent calls its page
tool. The background service worker owns Chrome tab leases, bounded page
inspection, reversible previews, saved site recipes, and explicitly requested
current-site cookie capture/restore. There is no parallel extension backend,
native host, or extension-to-process protocol.

## MVP flow

1. Click the Nanocodex toolbar action to open one React side panel. Nanocodex
   can target any loaded, accessible HTTP(S) tab; without an explicit target it
   uses the active web tab captured when the page tool runs. If the active tab
   is an internal, loading, or unavailable page, the tool asks you to choose a
   supported tab instead of falling back to an older selection.
2. Connect Nanocodex. The hosted Connect popup reuses the canonical Nanocodex
   passkey account and explicitly asks for final replies, action summaries,
   conversation history, full run traces, ChatGPT-backed agent access, and the
   exact browser-cookie-sync resource used only by the explicit controls.
   The app-scoped grant is retained in extension-local storage and validated
   when the panel is reopened.
3. The Connect SDK opens the account-owned durable agent and exchanges the
   grant for a one-time ticket to its private tool-host socket. The extension's
   `cleanup` catalog is attached to that exact agent and grant. No OpenAI or
   ChatGPT credential enters extension storage or browser traffic.
4. Chat normally with the durable agent. The shared conversation rail can mint
   a newly approved durable conversation or reopen any conversation retained by
   this extension. The panel records each user turn, streams thinking and the
   reply, and exposes the shared steer/stop controls.
5. When a prompt names another tab, the hosted agent calls `list_tabs` and sees
   only bounded titles, origins, query-free URLs, per-window active state,
   same-window state, and opaque tab references, at most 50 per tool result,
   with an opaque cursor for more. It
   selects one unambiguous reference and calls `inspect`; when no
   tab was named it calls `inspect` without a reference and gets the current
   active web tab. `preview` and `revert_preview` remain attached to that
   exact document. Ordinary chat never claims or inspects a page.
6. Inspection returns at most 500 visible semantic DOM candidates and 60,000
   characters. It omits form values, storage, cookies, other tabs, subframes,
   and URL queries/fragments.
7. A recipe `{name, css, hide_selectors}` is validated and previewed as one
   removable style element. Model output can never inject JavaScript, HTML,
   event handlers, remote resources, or extension capabilities.
8. **Revert** removes the preview. **Keep for this site** verifies that Chrome
   still allows access to that HTTP(S) host, stores the recipe in
   `chrome.storage.local`, and installs a persistent dynamic content script only
   for origins with approved recipes.
9. Saved recipes are listed in the side panel. **Forget** removes the recipe
   from storage and every open matching tab, and unregisters future injection.
   Installation-wide HTTP(S) access remains until the extension is disabled or
   removed, or the user changes its site-access setting in Chrome.
   Both saved and forgotten state survive closing and reopening Chrome.
10. **Capture this site** is the only action that requests Chrome's optional
    `cookies` permission. It claims the exact active HTTP(S) document in the
    side panel's window, rejects incognito, resolves that tab's exact cookie
    store, and captures only cookies Chrome reports as applicable to the leased
    URL. The raw jar stays in background memory except for the one direct
    message-to-authenticated-Connect handoff; React/UI state sees only an opaque
    jar ID, ownership fence, and count. **Restore saved cookies…** rediscovers
    the newest jar for that exact origin/profile/store after panel reload and performs a
    two-phase, one-minute confirmation and warns before
    replacing cookies currently applicable to that same leased URL. Navigation,
    tab closure, another profile/browser instance, another store, or unsupported
    partition metadata fences the operation. Restore attempts to roll back the
    previous in-memory jar if Chrome rejects a replacement.

The selected document is represented by an extension-owned opaque lease. Open-
tab references are short-lived, turn-scoped, and never expose Chrome tab IDs.
Every tool call checks the lease; navigation or tab closure invalidates it and
cancels the active turn. Page requests continue to use Chrome's existing
logged-in session. Explicitly captured cookie values travel only between
Chrome's Cookies API and background memory/API messages; they never enter React
state, the DOM, the agent, a transcript, a model call, logs, or
`chrome.storage`. Closing/releasing the lease discards them, and a background
worker restart intentionally loses them.

`lib/cookie-sync.ts` defines the versioned cookie envelope and narrow
`CookieSyncTransport` interface. `lib/connect.ts` maps that seam to
`/v1/browser-cookie-jars` through the retained authenticated `Client.fetch`;
it never reads, constructs, or stores a bearer. Capture performs the PUT only
after its direct click. Restore materializes the encrypted server jar only
after its direct click, then hands it immediately back to background memory for
confirmation and apply. The envelope binds `profile_id` to the stable extension
browser instance and `store_id` to the leased tab's Chrome cookie store, while
every cookie also retains its own `storeId`.

Only one side panel in the Chrome profile can own the reverse-attached cleanup
host at a time. The panel keeps that browser-owned lock for its agent session,
so a second window cannot redirect an in-flight tool call to a different tab.
Disconnect performs an ordered shutdown after an active turn settles. On panel
unload, the page synchronously fences its tool dispatcher and initiates turn
cancellation, lease release, attachment closure, and lock release before Chrome
can destroy the document; Chrome also releases the Web Lock with the document.
Grants created by the earlier local-agent preview are discarded on reconnect
and require one fresh approval because they do not identify a durable agent.

## Build and check

```sh
npm ci --prefix js/nanocodex
npm ci --prefix extensions/chrome
npm test --prefix extensions/chrome
npm run build --prefix extensions/chrome
```

Load `extensions/chrome/.output/chrome-mv3` from `chrome://extensions` in
developer mode. Exercise prompt → inspection → preview → Revert, then repeat
and Keep on a local fixture page. Reload or open a second tab to prove the saved
recipe reapplies. Close and reopen Chrome to prove both the Connect grant and
recipe return. Inspect the extension page, selected-page console, failed
requests, and ticketed Connect WebSocket before considering a browser change
complete.

For cookie evidence, press **Capture this site** on a regular HTTP(S) tab and
verify the optional permission prompt appears only then. Change one applicable
cookie, restore with the destructive confirmation, and verify its full flags in
DevTools. Repeat after navigation, on another origin/store, and in incognito to
prove each fails closed. Inspect extension local/session storage, the panel DOM
and React tools, console, network, transcript, and model/tool traces to confirm
that no cookie value appears.

## Permissions and security boundary

- Required Chrome APIs: `scripting`, `sidePanel`, and `storage`. Broad matching
  host access already permits the Tabs API to expose matching HTTP(S) tab
  metadata, so Nanocodex does not also request the redundant `tabs` permission.
- Required network origin: the pinned Nanocodex Connect API. The passkey flow
  opens the canonical HTTPS Connect host as a top-level popup; it is not embedded
  and receives no extension host permission.
- Required HTTP(S) host access allows a named open tab to be inspected or
  changed without making the user revisit that tab for a second toolbar click.
  The model receives only the bounded tab catalog until it invokes `inspect`.
- Optional Chrome API: `cookies`. It is declared in `optional_permissions` and
  requested only inside the direct **Capture this site** or **Restore saved
  cookies…** click handlers. Denial
  leaves chat and recipes functional. It is never requested by startup,
  connection, a model/tool call, or the background worker.
- Deliberately absent: `nativeMessaging`, `tabs`, `debugger`, `webRequest`,
  downloads, clipboard, externally connectable pages, and remote code.
- The Connect dialog owns passkey approval. The extension retains only its
  app-scoped grant session in origin-local storage; content scripts cannot read
  extension-local storage, and one-time tool-host tickets are never retained. No
  OpenAI API key, ChatGPT OAuth token, cookie, or provider credential is stored
  by the extension. Only the non-secret browser instance identifier is retained
  to enforce the cookie profile fence.
- Preview and persistence run in Chrome's isolated world. The model sees only
  the narrow cleanup schema, safe open-tab metadata, and turn-scoped opaque tab
  references—never Chrome tab IDs, lease tokens, cookie values, or Chrome APIs.
