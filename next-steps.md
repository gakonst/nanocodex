1. refactor + fix CI / get preview environments in PRs - move back to CI driven deployment where the deployment is teed up in parallel with the tests
2. sync with codex latest

- Anything new re: compaction or other tools?
- the /btw bug needs to be fixed
- need to get in another round of evals

1. redo the account connection UX a la OAuth in new tab
   "App is requesting access to this stuff"
   Click "sign" -> passkey -> OK done end of flow
   If you click sign and you dont have the permissions then you get CTA'd
2. Get this working inside the paradigm website

- Connect your accounts
- WebMCP / NanocodexProvider
- ChatGPT-esque app built in
- Slack button 1-click install to your <..> - make it work with other platforms as well. Telegram, iMessage, WhatsApp, Slack. Single PLayer & Multiplayer.

Make scrolling change the demo you are seeing
Hook with agent swarms the pokemon thing

cloud infra for personal agents that you can take anywhere and connect to everything

okay so we finish the deployment productivity stuff,
deslop, refactor and shit - get us to stable core again but the core is not just
rust it's also the stuff we've come up with on top

rust + wasm
viem + react hooks
durability for cf workers running the agents -- these start to get stable as the
inference stack, incl cf monitorign and shit

then we get back into the account connection stuff

weird tools can be very unstable

- memory
- session

finally product UIs are the fastest moving ones
Chrome extension
World
Chatroom
other Chat SDK connectors

## JS package boundaries

Completed refactor scope:

1. Finish `nanocodex-tools`: move the remaining platform-neutral tool router,
   configuration, Code Mode/tool-result primitives, attachments, and hosted
   catalog down from `nanocodex`. `nanocodex` keeps the WASM integration and
   compatibility reexports and may import `nanocodex-tools`; the dependency
   must never point upward or reimplement a WASM-provided tool.
2. Extract the reusable hosted-tools protocol and transport-neutral broker
   state machine from `managed` into `nanocodex-tools/hosted`. Keep Cloudflare
   Durable Object/WebSocket persistence, account authority, and Connect grant
   enforcement in `managed`.
3. Move the pure durable-memory contracts, validation, tokenization, ranking,
   and preview logic into `nanocodex-tools/memory`. Keep `memory-scope`, durable
   storage, AI Search, and account scoping in `managed`.

Deferred package-boundary work remains: explicit Connect contracts/UI owners instead
of cross-app source imports, MCP target and OAuth relay ownership, shared
multiplayer protocol, credential-envelope crypto, and a separate portable
durability package. Revisit these after the three extractions above.
