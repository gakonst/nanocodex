# WoW addon browser host

Run the production addon UI and conversation Lua outside WoW, connected to the real Nanocodex account through the official local CLI login. This is an interactive host for the addon, with real account conversations and incremental model responses.

```sh
cd examples/wow/preview
bun install
bun run build
python3 backend-server.py
```

Open http://127.0.0.1:17843 on the same computer. Python requires `websockets>=15,<16`. Sign in with the official `nanocodex2 login` flow if account status is disconnected. The browser never receives the account credential. The listener is loopback-only and rejects foreign Host/Origin requests; do not expose it through an unauthenticated tunnel.

`build.mjs` copies unmodified Context, Core, Projects, Bridge and Client Lua modules from the addon and records their SHA256 hashes. Fengari executes those modules. `wow-api.lua` projects WoW frame methods into a browser frame tree and routes real callbacks. The browser transport replaces the game's pixel/keyboard carrier; the existing Python dispatcher, journal and durable WebSocket backend remain responsible for account actions and streaming.

Refresh loads the actual roster. Select an existing chat or create one, then Ask. Ask WoW creates a separate real conversation, but game context is unavailable in this host. Stop, history, project controls, panel drag, minimize, hide and draft persistence invoke the addon code. The frontend polls committed stream events every100ms, deduplicates event IDs and never automatically retries a send after uncertain delivery. A page only polls requests admitted by that page; it cannot consume stale outputs from another browser view.

## Tests

Run `python3 -m unittest discover -s ../tests -p test_preview_backend.py` for the backend boundaries. After building, `node parallel-browser.test.cjs` exercises independent chats using synthetic requests without contacting an account. These checks do not establish native WoW rendering or keyboard-carrier behavior.
