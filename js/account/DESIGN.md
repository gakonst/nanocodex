# Website direction

## North star

Nanocodex should feel like the product itself: small, direct, technical, and
surprisingly capable. The site is not a conventional SaaS landing page. It is
the shortest path from the thesis to a working agent and the evidence behind
it.

The clearest reference is [fx.sh](https://fx.sh). We are deliberately borrowing
its product-site grammar:

- one compact, top-left reading column;
- a terse product statement, install command, and measured status line;
- the real browser agent as the primary product demonstration;
- documentation-like prose instead of marketing sections;
- monospace typography, hairline borders, few colors, and large areas of
  intentional empty space; and
- navigation that names concrete product surfaces rather than company pages.

This is inspiration, not a requirement to preserve fx's implementation,
branding, wording, logo, exact measurements, or every interaction. Nanocodex
should be recognizably related in spirit and recognizably ours in execution.

The shared brand lockup is `Paradigm / nanocodex`: Paradigm provides the small
provenance mark and Nanocodex remains the product identity. Do not substitute
the Vercel triangle, fx wordmark, or a lookalike glyph.

## Product thesis

The first screen should make these facts obvious without a marketing preamble:

1. Nanocodex is a high-performance Codex SDK that runs anywhere.
2. Its optimized browser artifact is small and its Terminal-Bench performance
   is directly comparable with Codex on a completed retained workset.
3. The agent running immediately below the claim is real browser WASM.

Prefer proof over claims. A working terminal, exact API snippet, retained eval,
rendered diff, or repository file is more valuable than a feature card.

## Visual language

### Typography

Berkeley Mono is the voice of the site: navigation, headings, prose, code,
controls, labels, and the terminal. Do not pair it with a display serif. Keep
type restrained and small; get hierarchy from spacing, alignment, case,
contrast, and a limited weight range rather than oversized copy.

Use licensed, locally hosted Berkeley Mono webfonts when they are available.
Do not fetch fonts from a third party or commit unlicensed font files. Until the
files are supplied, keep a metrics-compatible system monospace fallback stack
so the design can be built without pretending the asset exists.

### Color and material

- Black and white do almost all of the work.
- Dark is the primary art direction; light mode is the exact tonal inverse, not
  a separate design.
- Use one accent sparingly for focus, links, active state, and live activity.
- Surfaces are flat. Separation comes from space and one-pixel rules.
- Avoid gradients, glass, shadows, pills, oversized radii, illustrations, and
  decorative dashboard chrome.

### Layout

- Anchor the experience to the top-left rather than centering a hero.
- Keep the primary reading measure narrow, roughly 640-720 px on desktop.
- Let the terminal be wider only when the interaction benefits from it.
- Empty space is structural. It should make the few important objects feel
  precise, not make the page feel unfinished.
- Mobile keeps the same document order and tone. It should not become a stack
  of generic cards.

### Motion and state

Motion explains state changes: terminal expansion, navigation, streaming, and
focus. Nothing loops merely to decorate the page. Respect reduced motion.

Never replace a complete surface with loading copy, a spinner, a skeleton, or
a Suspense placeholder. Keep the last complete interface, render nothing until
the first complete boundary is ready, and show a concise actionable state only
after an operation fails.

## Homepage shape

The default composition should be simple enough to describe in one breath:

1. Compact wordmark and alphabetic product navigation.
2. One small descriptor: `High-performance Codex SDK. Runs anywhere.`
3. The copyable shell installer:
   `curl -fsSL https://nanocodex.paradigm.xyz | bash`.
4. One compact metadata line: the optimized WASM gzip size, Nanocodex and Codex
   Terminal-Bench 2.1 sol+high scores (`82.2%` and `79.6%`), completed-run count
   (`890/890`), and a direct link to the retained workset.
5. The real embedded Nanocodex agent, immediately after the metadata.

Do not put a proof grid, latency paragraph, provenance list, caveat block,
ownership essay, or capability section between the descriptor and demo. Do not
call the product experimental. Detail belongs in Docs, Source, Commits, and the
linked Evals workset.

The terminal is not a glossy demo frame or a video. It is the product proof. It
should be usable immediately, expand into the full agent surface, and retain its
session while the user moves around the site.

## Information architecture

Keep the global navigation short and alphabetic by displayed label:

- `Agent` — use the complete browser consumer;
- `Changelog` — read the immutable nightly commit record;
- `Commits` — read the implementation as rendered history;
- `Docs` — learn and integrate;
- `Evals` — inspect retained evidence;
- `Source` — browse the current internal Code surface; and
- a small theme control outside the product links.

The compatible `/code` route and internal `code` surface name remain, but the
visible label is `Source`; GitHub is a contextual external link rather than a
second global item named source. `Requests` stays out of primary navigation
until it is a real public workflow. The first letter of each product link is a
visible keyboard hint; the full active-label rule sits next to the text, not at
the distant bottom edge of the header.

Docs, agent, code, commits, and evals use one visual system. They may become
denser to serve their task, but they should not look like separate products.
Docs navigation uses bold active-page text without arrows or disclosure icons;
SDK subsection toggles remain semantic text buttons with `aria-expanded`.

## Nanocodex's own taste

Where fx presents a tiny native CLI, Nanocodex presents a composable agent
lifecycle. Our site should therefore lean further into:

- ownership diagrams and exact API contracts when they explain composition;
- the live browser workspace as a serious consumer, not a novelty;
- code, commit history, trajectories, verifier output, and performance records
  as first-class product material;
- clear separation between the small public SDK and application-owned policy;
  and
- quiet confidence: fewer superlatives, more exact behavior.

Do not inherit fx claims that conflict with Nanocodex. Nanocodex intentionally
supports one OpenAI model family and the Responses WebSocket API; provider and
model agnosticism are not goals.

## Content rules

- Lead with the product category and measured result.
- Treat evals as CI at explicit benchmark milestones; do not imply the normal
  GitHub workflow runs VM evaluation waves unless it does.
- Prefer short declarative sentences and concrete nouns.
- Use exact retained measurements only when their provenance remains linked
  and current. Round display values compactly, while keeping the linked workset
  as the exact evidence boundary.
- Do not say “blazing fast,” “production-ready,” “AI-powered,” or similar filler.
- Do not explain internal machinery before the public ownership model is clear.
- Reuse product language from the library documentation so the site does not
  invent a second contract.
- Derive live artifact sizes from the artifact being served. Date and link
  retained performance measurements. Never present a build ceiling as a
  measured bundle size.

## Interaction and performance gates

- Every navigation surface must be directly addressable, keyboard reachable,
  and useful at 390 x 844 CSS pixels without document-level horizontal scroll.
- Touch text inputs use at least 16 px text and 44 px controls. The browser
  agent must accept real iPhone Safari keyboard, paste, composition, multiline,
  and submit input through a visible native composer; an off-screen xterm
  helper textarea is not the mobile product boundary.
- Route changes preserve the last complete surface until the replacement is
  complete. Failures retain complete data where possible and name the failed
  operation with a retry action.
- Homepage, Docs, Source, Commits, and Evals each receive a transfer, request,
  first-complete-paint, interaction, DOM, and retained-memory budget measured
  on representative data. “Fast” is not copy; it is a gate.
- Production repository reads reuse the existing immutable generation and
  object caching. Commits remain one aggregate patch stream rather than an
  N-request-per-commit design. Evals avoid duplicate coordinate scans and
  suspend polling while the document is hidden.

## First simplification pass

The first implementation pass should reduce before it adds:

1. Replace the serif/semimono split with the Berkeley Mono-first type system.
2. Recompose the homepage into the narrow descriptor/install/metadata/terminal
   sequence.
3. Remove proof blocks, capability copy, and product-surface grids from the
   homepage; the named product surfaces carry that detail.
4. Reduce the global navigation and move secondary proof surfaces into context.
5. Preserve the real terminal lifecycle and every complete no-loading-state
   boundary while changing presentation.

## Success test

A technically literate visitor should understand the thesis in ten seconds,
run or expand the browser agent without hunting, find the integration path in
one click, and verify a serious claim in code or eval evidence. The page should
still feel distinctive when reduced to typography, rules, copy, and the live
terminal—because those are the design.
