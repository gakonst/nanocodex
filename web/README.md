# nanocodex

A from-scratch Codex rewrite for the latest generation of models. The experiment is to keep the
same tools and behavior while making the runtime much smaller; nanocodex makes the implementation
and evaluation record legible.

## Stack

- Vite + React
- Cloudflare Vite plugin and Workers runtime
- Wrangler for preview and deployment
- Pierre Trees and Diffs for the file tree, source viewer, and the single virtualized commit stream
- TanStack Virtual for the commit quick-jump and evaluation indexes
- Derived job, trial, trajectory, and verifier views

The visual system follows the local Paradigm website's semantic tokens,
typography roles, grid, controls, and search treatment while using system font
fallbacks rather than the site's proprietary font files.

## Development

```bash
cd web
npm install
npm run dev
```

The homepage consumes the publishable `nanocodex` and `nanocodex-react`
packages under `../js`; it does not reach into generated WASM artifacts. Its
React integration follows an external-store pattern: create a
`createNanocodexConfig()` once, pass it to `NanocodexProvider`, and consume
`useNanocodexState`, `useNanocodexMessages`, or the compatibility
`useNanocodex` hook. React owns no agent history or model-loop state.

The local Worker and Vite client run together at `http://localhost:5173`, using
the same Cloudflare Vite-plugin layout as Tempo's React MPP examples.

`npm run dev` and `npm run build` first regenerate
`src/data/harness-repository.json` from the parent repository. Override the source or
history depth with `NANOCODEX_REPO` and `NANOCODEX_COMMIT_LIMIT`. The default index
covers the complete repository history and stores it as one streamed patch
asset. The commit view parses complete files in bounded batches and appends
them to one Pierre CodeView, yielding between batches so scrolling stays
responsive.

The same sync step discovers linked worktrees and derives a compact eval index
from their retained Nanocodex and Codex jobs. It automatically pairs the largest
exact task-set match for a like-for-like comparison. Trial
details retain metrics, phase timing, verifier assertions, tool status, and
API-visible final output. Raw tool output, stderr, secrets, and hidden reasoning
are not copied into the website.

The homepage and Evals view present the matched public task set as a provisional
development instrument, not a general product ranking. The comparison links
each disagreement back to its retained trial so compatibility gaps can be
inspected directly.

The homepage is also a real embedded-agent demo with three deliberately thin
layers:

- `../js/bindings` publishes `nanocodex`, the viem-like imperative client. Its agent and turn
  handles expose prompt, typed browser content, steer, cancel, latest-checkpoint
  fork, historical-checkpoint fork, and clean sibling spawn operations.
- `../js/react` publishes `nanocodex-react`, the wagmi-like headless React owner. Its provider and
  hooks manage the module Worker lifecycle, readiness, commands, and event
  subscriptions without imposing presentation policy.
- `AgentTerminal` is the optimized Ratatui-faithful consumer: native colors,
  rendering hierarchy, queue/steer behavior, `/btw`, historical branch editing,
  branch navigation, per-branch drafts, clipboard images, and key bindings over
  virtualized transcripts.

The module Worker loads the generated `nanocodex-wasm` package, and the Rust
engine owns the persistent Responses session, typed history, event stream, and
tool loop. The Cloudflare Worker upgrades `/api/responses` and proxies OpenAI
tool calls. It accepts a user-provided OpenAI key into a one-hour Durable Object
session and returns only an opaque `HttpOnly`, `SameSite=Strict` cookie. The key
is never placed in a URL, local storage, React state, or WASM configuration.
A user key takes precedence over the optional deployment-owned
`OPENAI_API_KEY`; forgetting or expiring it falls back to that deployment key
when present.

Local development reads the optional ignored root `.env` through the repository
workflow. For a shared demo fallback, configure the deployed Worker with
`wrangler secret put OPENAI_API_KEY`. BYOK itself uses the `BYOK_SESSIONS`
Durable Object binding declared in `wrangler.jsonc` and does not require a
deployment-owned OpenAI key.

Streaming events are coalesced once per animation frame before updating the
semantic transcript, and each independently scrolling transcript is
virtualized. `npm test` keeps the
event accumulator bounded under a 20,000-delta burst and covers assistant,
reasoning, and tool lifecycle updates.

The homepage also exposes the release contract: the checksum-verifying install
command, in-place `nanocodex update`, the crates.io SDK entry point, and links
to the latest GitHub Release and grouped conventional-commit changelog. GitHub
release notes also credit each pull request contributor.

Navigation stays available whenever an input is not active: `H`, `T`, `C`, `R`,
and `E` switch between Home, Code, Commits, Requests, and Evals. The repository
homepage is the root route. In Code, `Ctrl+P` searches the left tree and `Ctrl+F` opens the
fuzzy all-file jumper. In Commits, `F` searches history. Code and commit
scrolling are left to Pierre CodeView and the browser's native input behavior.

## Production

```bash
npm run build
npm run preview
npm run deploy:preview
npm run deploy
```

The proposal endpoint is intentionally a testnet-preview `402` until a live MPP
recipient and settlement policy are configured.
