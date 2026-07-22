# TUI notes

This document is the durable working set for Nanocodex's Ratatui consumer. It
records current behavior, the evidence retained from Amp and Codex, measured
performance constraints, and ideas that still need filtering. It is not a
commitment to implement every candidate.

Nanocodex remains a headless, library-first SDK. The TUI is a thin consumer of
the owned agent/session API and must not reshape the public library contract or
grow into an app-server protocol, approval system, or generic scheduler.

## Keybindings and commands

The implementation in `bin/nanocodex/src/tui/mod.rs` is the source of truth.

### Submission and pane control

| Input | Current behavior |
| --- | --- |
| `Enter` | Submit to the focused pane. While that pane is running, request a steer at the next safe model/tool boundary. |
| `Tab` with a non-empty draft | Explicitly queue the draft as a follow-on prompt. |
| `Tab` or `BackTab` with an empty draft | Toggle focus between the main and `/btw` panes when a side pane exists. |
| `Shift+Enter`, `Alt+Enter`, or `Ctrl+J` | Insert a newline. |
| `Esc` while idle | Clear the draft. |
| `Esc` while a steer is pending | Cancel the focused turn and immediately submit all pending steers as one FIFO-ordered follow-up. Preserve the current composer draft and separately queued prompts. |
| `Esc`, then `Esc` within one second while running | Cancel the focused turn. The first press arms target-scoped confirmation and preserves the draft. Repeated key events do not confirm cancellation. |
| `Ctrl+C` | Quit. |
| `Ctrl+D` with an empty draft | Quit. |

`Enter` and `Tab` deliberately differ while work is active: `Enter` is a steer
that may affect the current run, while `Tab` creates a later queued turn.

### Editing and history

| Input | Current behavior |
| --- | --- |
| `Left` / `Right` | Move the cursor by one Unicode scalar. |
| `Up` / `Ctrl+P`, `Down` / `Ctrl+N` | Move by one visual composer row, preserving the preferred display column across short and wrapped lines. Moving above the first visual row enters non-wrapping transcript prompt selection without replacing the draft; Down past the newest selected prompt returns to the composer. |
| `Home` / `Ctrl+A` | Move to the start of the current input line. |
| `End` / `Ctrl+E` | Move to the end of the current input line. |
| `Backspace` / `Delete` | Delete before/under the cursor. |
| `Ctrl+B` / `Ctrl+F` | Move left/right by one Unicode scalar. |
| `Alt+B` / `Ctrl+Left`, `Alt+F` / `Ctrl+Right` | Move backward/forward by one word. `Alt+Left` and `Alt+Right` are accepted too. |
| `Ctrl+W` | Delete the previous word. |
| `Ctrl+U` / `Ctrl+K` | Delete to the start/end of the logical line; at a line boundary, delete the adjacent newline. |
| `Ctrl+G` | Open the current draft as Markdown in `$VISUAL`, falling back to `$EDITOR`, then replace the composer with the saved text. |
| `e` while a prior prompt is selected | Replace that user-message row with a bordered inline editor while preserving the surrounding transcript and the current composer draft, including while its branch is still running. `Enter` forks immediately before the prompt and sends the revision on the new branch; the original completion continues on its retained branch. `Esc` cancels, clears selection, and restores composer focus. `Shift+Enter` inserts a newline and `Ctrl+G` edits the inline buffer in `$VISUAL`/`$EDITOR`. |
| `Ctrl+Alt+Up` / `Ctrl+Alt+Down` | Cycle through retained main branches. Each branch preserves its transcript and composer draft. |
| `Ctrl+Alt+B` | Toggle the right-side branch tree. Parent/child connectors and nesting show fork topology; `Up`/`Down` or `j`/`k` instantly preview and switch in depth-first tree order, and `Esc` closes it. During a running turn the preview still changes immediately and the agent switch follows when the turn becomes idle. |
| Terminal paste | Insert literal pasted text at the cursor after normalizing CRLF and CR to LF. |

### Transcript navigation

| Input | Current behavior |
| --- | --- |
| `PageUp` / `PageDown` | Scroll the focused transcript by 12 rows. |
| Mouse wheel | Scroll the focused transcript by 3 rows. |
| `Ctrl+End` | Jump the focused transcript to the newest output. |

### Slash commands

| Command | Current behavior |
| --- | --- |
| `/btw <question>` | Fork the latest safe mainline checkpoint into a side pane and submit the question there. Partial model output and unmatched tool calls are excluded. |
| `/btw` | Open an empty side fork, or focus the existing side pane. |
| `/close` | Close the `/btw` pane once it is idle. A busy pane is retained and reports why it cannot close. |
| `/cancel` | Cancel the focused turn without the two-stage Escape gesture. |
| `/trace` | Open Jaeger filtered to the focused session. A `/btw` trace becomes available after its fork has produced a session ID. |

Unknown slash-prefixed input is sent to the model as an ordinary prompt.

## Current design and retained practices

### State and rendering

- Transcript state is semantic: user, assistant, tool, and error entries are
  retained separately from their rendered cells.
- Streaming assistant deltas append to one canonical raw Markdown tail rather
  than adding one transcript entry per delta. Rendering temporarily closes
  incomplete fences, inline code, emphasis, strikethrough, and links without
  mutating that source; the final assistant event disables healing and seals
  the exact completed response.
- Tool state is updated by call ID and distinguishes running, completed,
  cancelled, and failed calls.
- Markdown supports headings, emphasis, lists, quotes, links, fenced code, and
  tables. Known fenced-code languages use a lazily loaded native syntax set;
  unknown languages retain a plain-code fallback. Tables keep aligned columns
  when they fit and become labeled row cards at narrow widths.
- Code Mode's `parent/code-N` events render as one activity tree. Child calls
  retain status, compact arguments/results, and duration; the completed parent
  uses its wall time and the summed child durations to label demonstrably
  overlapping work without parsing or guessing at the JavaScript program. The
  parent JavaScript remains multiline and syntax-highlighted, while multiline
  child commands render as indented continuation rows instead of one flattened
  line. Immutable highlighted source is cached on the activity.
- `apply_patch` activities retain bounded patch source and render operation-aware
  file paths, moves, aggregate and per-file `+/-` counts, hunk markers, and
  syntax-aware added/context lines instead of flattening a patch into arguments.
- Following Codex's composer contract, explicit pastes over 1,000 characters
  become compact `[Pasted Content N chars]` elements. Their full text stays out
  of layout and rendering, equal-sized pastes receive stable suffixes, element
  deletion is atomic, and surviving placeholders expand losslessly on submit.
- Wrapped entry height is cached for the current terminal width. A width change
  recomputes it.
- Rendering finds the visible window from the newest entry backward, then clips
  only those entry paragraphs. Tail work is proportional to visible rows and a
  deliberate reading offset rather than complete retained history. Ratatui's
  buffer diff then writes only changed cells.
- Main and `/btw` conversations own independent transcripts, queues, statuses,
  and scroll offsets. The composer targets whichever pane has focus.
- Composer measurement, rendering, cursor placement, and vertical motion use
  one hard-wrapped display-row map. This keeps exact-width boundaries and wide
  Unicode characters consistent instead of asking Ratatui to apply a second,
  different word-wrap policy. The composer retains its visual-row viewport:
  cursor motion moves within the box first, and content scrolls only when the
  cursor crosses its top or bottom edge.
- External editing follows the reviewed Codex lifecycle: resolve the editor to
  argv, seed a temporary `.md` file, drop the terminal event reader, restore
  normal terminal modes while the editor owns stdin, and recreate the reader
  after the TUI resumes. The child receives `NANOCODEX_EXTERNAL_EDITOR=1` so
  editor configuration can suppress project-oriented startup UI for the
  temporary draft. A launch or editor failure preserves the draft.
- A scrolled pane captures the cached wrapped height of a changing tail once
  per frame burst. The next render adds only the coalesced wrapped-row growth
  to its bottom-relative offset, so streamed text and new entries do not move
  the reading window. The pane title marks unseen output until `Ctrl+End` or
  scrolling back to the tail. Main and `/btw` anchoring state is independent.
- Resize retains the current wrapped-row distance from the tail; later output
  is measured at the new pane width. This avoids a full-history reflow solely
  to manufacture a semantic resize anchor.
- Manual scroll offsets are clamped to the transcript's actual wrapped height
  at the current viewport size. Repeated wheel or page input at the oldest row
  therefore cannot accumulate invisible overscroll that must later be unwound.
- Mouse-wheel events are coalesced for one frame. Reversing direction within
  that queued burst discards the older direction instead of making the user
  wait for stale trackpad momentum to drain.
- Terminal setup uses synchronized updates, bracketed paste, mouse capture, and
  enhanced keyboard and focus reporting where supported. Completion emits one
  OSC 9 notification on known supporting terminals, with tmux passthrough, or a
  BEL fallback only while unfocused. Restoration is drop- and panic-safe.

### Scheduling

- Rendering is demand-driven rather than a permanent full-speed loop.
- Streaming events are coalesced behind an approximately 120 Hz maximum frame
  rate (`8.333334 ms`).
- Input and resize request an immediate frame and preempt a pending streaming
  deadline.
- The retained Codex workload's densest 33 ms bucket contained 590
  display-affecting records; the scheduler reduces that burst to frame-rate
  work rather than one render per record.

### Observability

TUI telemetry carries a private process-monotonic source timestamp beside each
typed event without changing the public event or JSONL contract. It correlates
socket receipt, agent emission, TUI receipt, state application, Ratatui diff
rendering, and terminal flush. Frame records include coalesced delta count,
payload bytes, changed cells, output bytes, render duration, and first/last
source-to-presentation latency. Compact per-turn summaries are exported at
`info`; `just run-otel-detail` enables individual records. Full conversation
content remains in the agent lifecycle traces described in
`docs/OBSERVABILITY.md`.

## Representative workload evidence

TUI work should be evaluated against retained representative workloads rather
than visual intuition alone. Raw traces and Amp exports stay outside Git;
committed fixtures may retain deterministic structural summaries only.

The benchmark shapes below were derived on 2026-07-20 from a long local Codex
rollout and the longest thread then returned by
`amp threads list --include-archived --json`. No prompts, arguments, results,
or other user content were retained.

| Shape | User messages / chars | Assistant messages / chars | Tool calls / argument chars |
| --- | ---: | ---: | ---: |
| `codex_long` | 78 / 30,486 | 964 / 308,701 | 3,471 / 1,438,038 |
| `amp_long` | 38 / 4,716 | 199 / 69,676 | 241 / 162,209 |

The Criterion suite in `bin/nanocodex/benches/tui_render.rs` measures:

- tail and 4,000-row-scrolled trace rendering at `80x24`, `120x40`, and
  `200x60`;
- alternating `80x24`/`200x60` resize reflow;
- first-frame construction at `120x40`;
- a streaming delta appended to a 2 KiB assistant tail;
- a 128-delta burst applied while scrolled, its coalesced anchor settlement,
  and the complete anchored frame at `120x40`;
- a 128-row follow-bottom burst, one smooth viewport step, and draining the
  bounded animation backlog at `120x40`;
- repeated rendering of a 100 KiB multiline composer draft at `120x40`; and
- 100 KiB large-paste ingestion, placeholder rendering, and submission expansion;
- first-frame rendering of a 16-file patch activity; and
- selection of every retained user prompt and the first selected-history frame
  at `120x40`;
- finalized Markdown parsing plus its first `120x40` frame;
- a healed streaming Markdown update and `120x40` frame; and
- a 16-child Code Mode activity update plus its `120x40` frame.

On the 2026-07-21 development-machine gate, sealing and first-rendering a
representative multi-section Markdown fixture with ten highlighted Rust blocks
measured 1.285 ms. Healing and rendering the same fixture with an incomplete
formatted tail measured 1.313 ms per changed frame. The scheduler coalesces
multiple deltas before that work. Updating and rendering a 16-child Code Mode
tree with cached highlighted JavaScript measured 171.87 µs.

`TUI-PERF-01` replaced the complete oldest-first height sum and walk with a
bottom-up visible-window search. On the 2026-07-21 development-machine run,
`codex_long` tail frames improved from 0.46–1.07 ms to 0.19–0.54 ms across the
three sizes; `amp_long` tail frames improved from 0.23–0.60 ms to 0.19–0.54 ms.
The 4,000-row cases were unchanged or faster. Alternating-width resize reflow
dropped from 35.30 ms to 0.96 ms for `codex_long` and from 16.65 ms to 0.49 ms
for `amp_long`. A deterministic equivalence test compares the optimized window
against the former complete-height algorithm across widths, heights, offsets,
wrapping, and over-scroll clamping.

`TUI-SCROLL-01` adds only affected-tail/new-entry height work. On the 2026-07-21
development-machine run, 128 deltas applied to a cached 2 KiB scrolled tail in
8.70 µs, their one coalesced wrapped-height settlement took 126.50 µs, and the
complete anchored `120x40` frame took 343.14 µs. Deterministic tests preserve
the rendered reading window across wrap growth and cover new entries, resize,
main/`/btw` isolation, unseen-output clearing, and the explicit jump action.

`TUI-STREAM-01` was evaluated but not implemented. A benchmark-only prototype
compared the current incrementally maintained styled tail with raw-string
accumulation followed by one allocation-bearing materialization per frame.
Although 128 raw appends were faster in isolation (0.734 µs versus 3.610 µs),
the repeated end-to-end update-and-render result was 414.43 µs versus 417.34 µs
for the current representation, a statistically insignificant difference.
Finalized entries already stop invalidating because only the active assistant
tail is mutable. Explicit sealed/raw state is therefore deferred; canonical raw
assistant source remains a prerequisite to design and benchmark with the
Markdown rendering slice rather than an independently justified abstraction.

`TUI-COMPOSER-01` unified composer measurement, drawing, cursor placement, and
visual-row motion behind one display-row layout, and added the focused readline
bindings plus `Ctrl+G` external editing. On the 2026-07-21 development-machine
run, the 100 KiB multiline `120x40` frame improved from a 3.53 ms baseline mean
to 0.56 ms (84% faster). Deterministic tests cover logical and wrapped vertical
motion, preferred-column restoration, exact wrap boundaries, wide characters,
line/word deletion, editor argv parsing, and the temporary-file round trip. The
retained-viewport follow-up measured 0.30 ms on the same gate, so edge-triggered
composer scrolling introduced no frame-time regression.

Prompt selection stays virtualized by semantic entry rather than converting the
selection into a bottom-relative row offset. The discarded offset prototype
took 32.72 ms to select and render the latest prompt in `codex_long` and 3.58 ms
in `amp_long` because it cold-reflowed the intervening transcript. The earlier
direct-entry implementation reached 0.30 ms and 0.28 ms but incorrectly cleared
the context above the selected row. Preserving the existing viewport when the
row is visible and revealing it with one row of padding otherwise measures
0.365 ms and 0.356 ms. Rendering the bordered inline editor measures 0.363 ms
and 0.353 ms. Traversing all retained prompts takes 2.15 µs for `codex_long`
(78 prompts) and 0.86 µs for `amp_long` (38 prompts).

`TUI-BRANCH-01` retains opaque completed `TurnResult`s in the TUI worker and
uses `fork_from` with the result immediately before the selected prompt. Editing
the first prompt uses a clean sibling because no earlier checkpoint exists.
Transcript prefixes share immutable entry allocations; abandoned branches keep
their agent, transcript, and composer draft. On the 2026-07-21 retained-shape
gate, creating the visible fork prefix took 2.82 µs for `codex_long` and 1.69 µs
for `amp_long`; switching branches took 0.50 µs and 0.31 µs respectively. The
right-side tree navigator frame takes 0.307 ms and 0.257 ms. A
wire-level regression test proves that editing the still-running second prompt
sends only its replacement with the first response as `previous_response_id`,
while the original completion continues on the independently selectable parent
branch. The header renders a compact
`child←parent` graph and marks the active node with `*`.

Run it with:

```sh
cargo bench -p nanocodex-bin --bench tui_render
```

Use `just bench-stream` for the focused cross-layer gate. It also measures the
timed agent-event envelope in `nanocodex-service` and the TUI timing aggregator,
so UI improvements are not credited for delays introduced before Ratatui
receives an event or for unmeasured instrumentation overhead.

Every performance slice should select applicable gates before implementation:

- event/state-update throughput;
- frame construction and layout time;
- frames rendered per event burst;
- changed-cell count and terminal output volume;
- allocations and retained memory;
- input-to-frame latency; and
- resize/reflow behavior.

Validate claimed improvements at multiple terminal sizes and at both the
streaming head and long-history tail. Use a focused synthetic case only to
isolate a demonstrated boundary.

## Amp findings retained

What survived from the Amp reverse-engineering work:

- The two-stage, one-second Escape gesture was adopted. Cancellation is scoped
  to the focused target, repeated key events do not accidentally confirm it,
  and an in-progress draft is preserved.
- Immediate steer and explicit queue are separate input intents.
- The side-question flow informs `/btw`: a question can branch from a safe
  checkpoint without interrupting the main line.
- Mature, long interactive threads informed the `amp_long` workload shape,
  particularly message wrapping, tool density, and long-session behavior.
- Input history, multiline composition, visible pending input, and concise
  footer hints are treated as daily-driver behavior rather than decoration.

What did not survive:

- There is no consolidated prose export of the earlier Amp discussion.
- The exact Amp thread ID used to derive `amp_long` was not retained.
- The current Amp thread titled "Nanocodex assistance" does not contain the
  missing research.

The structural benchmark summary and adopted behavior are therefore the durable
evidence. Future Amp research should record the export ID, date, observations,
and any sanitized derived fixture here while keeping the raw export outside
Git.

## Codex reference ideas

These are ideas observed in the local Codex checkout at the reviewed upstream
checkpoint `openai/codex@35eaf3ffb0bf2001486c68c47a3d946b34d16634`.
They are evidence and design input, not API requirements or automatic parity
work. The local checkout may be newer; advancing the reviewed checkpoint still
requires classifying every later upstream commit.

Relevant reference areas under `~/github/openai/codex/codex-rs/tui/src`:

- `external_editor.rs` and `app/input.rs`: parse `$VISUAL`/`$EDITOR`, round-trip
  a temporary Markdown file, and run the editor with the terminal restored.
- `bottom_pane/textarea.rs` and `keymap.rs`: keep wrapped-row ranges shared by
  rendering/cursor motion and provide the standard readline bindings.
- `app/agent_message_consolidation.rs`: consolidate transient streamed cells
  into canonical finalized message source while preserving resize re-rendering.
- `app/resize_reflow.rs`: explicit reflow state and resize behavior.
- `app/history_ui.rs`: history cells and terminal-native scrollback.
- Paste-burst handling that treats a large paste as one interaction instead of
  a rapid sequence of normal keypresses.
- Smooth streaming that drains display work at a controlled cadence.
- Markdown rendering, diff rendering, and table-aware presentation.
- Status indicator/shimmer and completion notifications such as BEL or OSC 9.
- Message-history lookup and rebuilding scrollback after clear or rollback.
- Compatibility handling for terminals with materially different scrollback or
  escape-sequence behavior, including Terminal.app and Warp.

Do not import Codex's app server, approval flow, generic history manager, or
multi-agent scheduler. Nanocodex should copy useful invariants and operational
behavior while retaining its much smaller consumer surface.

## Candidate backlog

Candidate IDs are stable handles for later filtering. `Now` means the idea fits
the current narrow Ratatui consumer; `Evaluate` requires evidence or a product
choice; `Defer` is intentionally outside the next slice.

| ID | Priority | Candidate | Evidence and acceptance boundary |
| --- | --- | --- | --- |
| `TUI-PERF-01` | Done | Add a long-history height/index strategy. | Bottom-up traversal is benchmarked on both representative shapes, scrolled and unscrolled, with alternating-width resize invalidation. The retained-history walk and sum are no longer on the normal tail-render path. |
| `TUI-SCROLL-01` | Done | Preserve reading position while output streams. | Wrapped growth and new entries are coalesced into the pane's bottom-relative offset. The title marks unseen output; `Ctrl+End` and reaching the tail clear it. Scroll input and resize clamp at the real wrapped extent without retaining hidden overscroll. Tests cover wrap growth, resize, clamping, and main/`/btw` isolation. |
| `TUI-STREAM-01` | Done | Make streaming versus sealed transcript entries explicit. | The assistant tail retains canonical raw Markdown plus explicit streaming state. Changed frames heal only their temporary parse input; the terminal event seals exact source and disables healing. The representative highlighted streaming frame is benchmarked under the frame budget. |
| `TUI-SMOOTH-01` | Done | Smooth follow-bottom movement once streaming output fills the viewport. | Keep agent events and canonical transcript updates immediate, but retain the prior visual bottom while newly wrapped rows arrive and advance it by one row per render. Initial viewport fill is unchanged, newline-heavy bursts retain their exact visual row debt instead of jumping when they exceed one screen, and manual scroll cancels pending automatic movement immediately. |
| `TUI-COMPOSER-01` | Done | Make multiline editing a reliable daily-driver surface. | `Ctrl+G` safely yields stdin and terminal ownership to `$VISUAL`/`$EDITOR`; readline keys work; rendering and Up/Down share one visual-row map. The 100 KiB composer frame is benchmarked and exact wrap boundaries are regression-tested. |
| `TUI-BRANCH-01` | Done | Make transcript edit create an Amp-style historical branch. | Up at the composer boundary selects prior user turns inline without wrapping or clearing a running response. `e` replaces the selected row with an inline editor; submit may fork while the source completion continues on its retained branch and sends only after the new branch opens. Abandoned branch agents, transcripts, and drafts remain available. `Ctrl+Alt+B` opens the measured right-side depth-first branch tree; moving its selection immediately changes the transcript preview and switches the idle agent without an Enter confirmation. `Ctrl+Alt+Up/Down` retains fast cycling. |
| `TUI-PASTE-01` | Done | Handle very large explicit pastes deliberately. | Match Codex's greater-than-1,000-character threshold and placeholder labels, keep full normalized text outside composer layout, make placeholder deletion atomic, and expand surviving payloads exactly on submission. The 100 KiB ingest/render/expand path is benchmarked. Non-bracketed key-event paste detection remains unnecessary while the Ratatui terminal contract enables bracketed paste. |
| `TUI-RENDER-01` | Done | Render assistant Markdown and useful tables. | Streaming snapshots heal incomplete constructs before parsing; completed source is exact and width-cached. Wide tables align columns, narrow tables become lossless labeled row cards, and fenced blocks receive native syntax highlighting with a plain fallback. Deterministic healing/reflow/highlighting tests and finalized/streaming frame benchmarks cover the boundary without changing the transcript event contract. |
| `TUI-TOOL-01` | Done | Improve tool-call presentation. | Code Mode parent and `/code-N` child events form one timed activity tree with multiline highlighted JavaScript, multiline child continuation rows, compact results, explicit status, failure detail, and evidence-based sequence/overlap labels. Patch calls add operation-aware paths, moves, `+/-` counts, and styled hunks. A 16-child update/frame benchmark and 16-file patch frame cover the cached representation. |
| `TUI-NOTIFY-01` | Done | Notify on completion while unfocused. | Focus reporting gates exactly one terminal-safe completion notification per turn. Known terminals receive OSC 9, tmux receives an escaped passthrough sequence, unknown terminals use BEL, and a backend write failure disables later attempts. |
| `TUI-SEARCH-01` | Evaluate | Add transcript search and copy-oriented navigation. | First define how matches interact with semantic entries, wrapped rows, two panes, and streaming updates. |
| `TUI-BTW-01` | Defer | Support multiple named `/btw` panes. | Product candidate already recorded in `docs/ORCHESTRATION_DECISION_CONTEXT.md`; preserve fresh driver/tool runtime ownership and explicit cleanup. This is broader than a rendering slice. |
| `TUI-BTW-02` | Evaluate | Make branch cancellation and close cleanup explicit. | Cancellation exists, but busy `/close` is rejected. Decide whether close should offer cancel-and-close while guaranteeing subprocess and driver cleanup. |
| `TUI-SNAPSHOT-01` | Defer | Restore durable conversations in the TUI. | Depends on the library's durable serializable conversation snapshot contract; the TUI must consume rather than invent it. |

## Suggested order

1. [x] Baseline and implement `TUI-PERF-01`.
2. [x] Specify and implement scroll anchoring and unseen-output behavior in
   `TUI-SCROLL-01`.
3. [x] Evaluate `TUI-STREAM-01`; defer explicit sealed entries because the
   benchmark did not demonstrate an end-to-end win.
4. [x] Implement and benchmark `TUI-COMPOSER-01`.
5. [x] Complete checkpoint-backed edit/branch switching and its compact,
   measured branch navigator in `TUI-BRANCH-01`.
6. [x] Smooth follow-bottom viewport movement without delaying canonical agent
   events or initial viewport fill in `TUI-SMOOTH-01`.
7. Choose one interaction slice from paste, Markdown, tool presentation, or
   notifications based on representative-session evidence.
8. Revisit multiple `/btw` panes only after the single-pane lifecycle and
   cleanup behavior are unambiguous.

## Source map

- Current input behavior: `bin/nanocodex/src/tui/mod.rs`
- Composer display rows: `bin/nanocodex/src/tui/composer.rs`
- External editor round trip: `bin/nanocodex/src/tui/external_editor.rs`
- TUI state and Amp Escape invariant: `bin/nanocodex/src/tui/app.rs`
- Transcript rendering and height cache: `bin/nanocodex/src/tui/transcript.rs`
- Layout and footer help: `bin/nanocodex/src/tui/view.rs`
- Render scheduling: `bin/nanocodex/src/tui/scheduler.rs`
- Terminal setup/restoration: `bin/nanocodex/src/tui/terminal.rs`
- Timing instrumentation: `bin/nanocodex/src/tui/telemetry.rs`
- Representative benchmarks: `bin/nanocodex/benches/tui_render.rs`
- TUI performance policy: `AGENTS.md`
- Branching context: `docs/ORCHESTRATION_DECISION_CONTEXT.md`
- Trace behavior: `docs/OBSERVABILITY.md`

Relevant implementation history:

- `f65e358 feat(cli): add ratatui daily driver`
- `6329113 perf(tui): coalesce streaming renders`
- `3c310bc test(tui): cover escape cancellation`
- `3957fb7 fix(tui): distinguish cancelled tools`
- `15a4c1d fix(tui): suppress cancellation error rows`
- `4adccc5 fix(tui): reconcile pending steer state`
