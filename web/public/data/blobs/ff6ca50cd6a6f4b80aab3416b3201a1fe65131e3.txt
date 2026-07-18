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

The local Worker and Vite client run together at `http://localhost:5173`, using
the same Cloudflare Vite-plugin layout as Tempo's React MPP examples.

`npm run dev` and `npm run build` first regenerate
`src/data/harness-repository.json` from the parent repository. Override the source or
history depth with `HARNESS_REPO` and `HARNESS_COMMIT_LIMIT`. The default index
covers the complete repository history and stores each patch as a separate
asset. The commit view parses them in bounded batches and appends them to one
Pierre CodeView, yielding between batches so scrolling stays responsive.

The same sync step discovers linked worktrees and derives a compact eval index
from their retained Harness and Codex jobs. It automatically pairs the largest
exact task-set match for a like-for-like comparison. Trial
details retain metrics, phase timing, verifier assertions, tool status, and
API-visible final output. Raw tool output, stderr, secrets, and hidden reasoning
are not copied into the website.

The homepage and Evals view present the matched public task set as a provisional
development instrument, not a general product ranking. The comparison links
each disagreement back to its retained trial so compatibility gaps can be
inspected directly.

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
