# Unified TUI plan: the nanocodex2 TUI becomes the only TUI

Status: planning only. No tracked files were edited.
Baseline: HEAD 0ca1d9496 (= origin/master at the time of writing) in worktree `unify-nanocodex`.
Paths:
- `N2` = `bin/nanocodex/src/nanocodex2/tui/` today. Stage A (agent 7) may move this tree when it merges the binaries. Every package rebases on Stage A's final module root and keeps the relative file names used below.
- `L` = `bin/nanocodex/src/tui/`, the legacy TUI. It is deleted at the end.

Owner decisions: one TUI with the union of both feature sets. Jaeger and `/trace` are removed everywhere. `ncl` (or `nanocodex --local`) is LOCAL; bare `nanocodex`, `nc` and `nanocodex2` are MANAGED (CONTRACT.md).

---------------------------------------------------------------------------------------------------
## 0. Facts the design relies on (verified in source)

- The managed driver is `N2/mod.rs::run_inner(client: &ManagedClient, attach)` (mod.rs:2094). `run`/`run_new` are at 2082-2092. `DriverRuntime` (mod.rs:553-668) owns `client: ManagedClient` and `agent: Option<Nanocodex>`, plus about 100 managed-specific fields.
- Local prompting already goes through the backend-generic handle. `start_submission` (mod.rs:1540-1575) calls `agent.prompt(PromptRequest…)`, which yields a `Turn`. Admissions (mod.rs:3551-3600) store `turn.control()` in `controls: HashMap<TurnId, TurnControl>`. `CancelTarget::Local` and `SteerTarget::Local` already exist (mod.rs:94-127).
- In managed mode the transcript is fed only from `ManagedEvent`. `connect_agent` (mod.rs:1940-2030) drops the generic `AgentEvents` (`let (agent, _events, ..)`). `TranscriptRecord::from_agent(seq, ts, AgentEvent)` exists (transcript/record.rs:162), as does `from_local` (record.rs:180). history.rs:399-406 already maps nested agent events this way.
- `N2/shared.rs` (389 LOC) is a second, complete driver over the same `AppNode`/`RootNode`. It is the template for `run_local`. It also shows how unsupported effects are handled: a catch-all `_ => {}` (shared.rs:370).
- Today's legacy entry is `L/mod.rs::run_observed(AgentArgs, VmArgs, Option<InitialPrompt>, Option<DurableSession>, Option<ObservabilityArgs>)` (L/mod.rs:820). It builds a `ConfiguredAgent` (config.rs:47-61) with `handle`, `events`, `claude_scheduler`, `claude_interactions`, `realtime: Option<OpenAi>`, `child_agents`, `subagent_updates`, `mpp_adapter`, `mcp: Option<McpHandle>`, `browser`, `vm` and `model`. It can rebuild the backend before the first prompt, which switches harness or model (`can_replace_backend`, L/mod.rs:827; rebuild at L/mod.rs:1140-1170).
- The local handle can fork. `Nanocodex::fork_from(snapshot)` and `spawn()` back legacy /btw (L/mod.rs:2856-2862), and `TurnResult::snapshot()` exists (crates/nanocodex-agent/src/agent/turn.rs:251).
- Slash commands in the N2 TUI are spread over several places:
  - `components/composer.rs`: `SettingsCommand` (60-85), `parse` (87-160) and `take_local_command` (1128-1200).
  - `components/keybindings.rs`: `BINDINGS: [(&str,&str); 39]` (22-75). The fixed array length means every addition conflicts.
  - `components/actions.rs`: the `Action` enum and its names (396-417).
  - `components/root.rs`: `RootEffect` (316-470) and its dispatch.
  - `mod.rs::apply_update` (4008-5200), which handles every effect.
- Legacy slash commands are in `L/slash_commands.rs:12-97`: /model /thinking /fast /btw /voice "/mcp login" "/mcp reload" /cancel /trace /benchmark /collapse /split /close.
- tui-control: legacy registers as `"native"` (L/startup.rs:171) and serves history.list/history.read from the rollout (L/control.rs:12, 191-210, 309). N2 registers as `"managed"` (N2/mod.rs:2285; N2/control.rs:75-199).
- Couplings outside `L` that block deletion:
  - config.rs:157 (`crate::tui::voice::validate_key`)
  - config.rs:169 (`crate::tui::ToolCalls`)
  - config/claude/interaction.rs:23 (re-exports `tui::interaction::PendingInteraction`)
  - main.rs:418-498 (resume and bare TUI)
  - eval/attach.rs:31 (`tui::attach_evaluation`)
  - eval/benchmark.rs:112-115 (`tui::run`)
  - benches/tui_render.rs (`include!`s L files)
  - tests/tui_split.rs (`#[path]` L/split.rs)

---------------------------------------------------------------------------------------------------
## 1. Local driver design (`run_local`)

### 1.1 Shape
Add a backend axis to the existing driver instead of writing a third full event loop. Reasons:
- `apply_update` handles about 60 `RootEffect`s.
- About 40 of them are backend-neutral: shell `!`, links, $EDITOR, copy/copy-response, theme, file finder, skills, recent prompts and prompt cache, queue/steer/withdraw via `TurnControl`, subagent tree, context diagnostics, effort/model/fast pickers, keybindings, zoom/panes.
- Duplicating that as shared.rs does would fork roughly 3k LOC.

New and changed files, all owned by WP0:

```
N2/backend.rs          (new)
  pub(crate) enum Backend { Managed(ManagedClient), Local(LocalBackend) }
  pub(crate) struct Capabilities { share, sites, vault, secure_input, screen, autoroute, done,
       connectors, bug, managed_sessions, managed_btw, reload, handoff, review_download,
       routing, voice_managed, voice_realtime, mcp, branches, collapse_split, claude_host, eval }
  impl Capabilities { const MANAGED; fn local(&ConfiguredAgentView) -> Self }
N2/local/mod.rs        (new)  pub(crate) async fn run_local(LocalLaunch) -> eyre::Result<()>
N2/local/agent.rs      (new)  LocalBackend: owns ConfiguredAgent parts (handle, events, mcp, browser, vm,
                              mpp_adapter, realtime, child_agents, subagent_updates, claude_* receivers),
                              shutdown() ported from L/mod.rs:1310-1350 (shutdown_runtime)
N2/local/events.rs     (new)  AgentEvents -> TranscriptRecord::from_agent -> AppEvent::Transcript; turn
                              completion -> AppEvent::WorkerTurnFinished; subagent updates
N2/local/updates.rs    (new)  enum LocalUpdate { Agent(..), Subagent(..), Feature(FeatureUpdate) } and an
                              unbounded channel; every feature task sends into it (one select! arm)
N2/features/mod.rs     (new)  FeatureEffect / FeatureEvent / FeatureUpdate enums with ALL variants this
                              plan needs pre-declared, and dispatch() that routes to per-feature files
                              (stub bodies return NotifyError "not yet available")
N2/commands.rs         (new)  slash-command registry (see section 3)
```

`DriverRuntime.client: ManagedClient` becomes `backend: Backend`. Accessors:
- `fn managed(&self) -> Option<&ManagedClient>`
- `fn require_managed(&mut self, pane, what) -> Option<ManagedClient>`. In local mode it emits `NotifyError("{what} needs a Nanocodex account session (run nanocodex)")`.

WP0 does this rename mechanically in mod.rs (about 40 `runtime.client`/`self.client` sites, listed by `grep -n client N2/mod.rs`). Managed behaviour must not change.

### 1.2 Startup (local)
`ncl` with no subcommand (Stage A's local tree, previously main.rs:490-498) calls `tui::run_local(LocalLaunch { args: AgentArgs, vm: VmArgs, initial_prompt, resume: Option<DurableSession>, observability })`. The same function serves:
- `ncl resume [ID] [--from ROLLOUT --at N] [--prompt]` (old main.rs:418-488)
- eval/benchmark.rs:112 (`/benchmark`)

`run_local` then:
1. Applies `config.prefer_codex_for_vm(&vm)` (L/mod.rs:822).
2. Resolves the workspace from `AgentArgs::cwd()` or the resumed session. It never uses `HostConfig::load()`, because local mode needs no account.
3. Paints the first frame immediately with `RootNode::new` and `set_capabilities(Capabilities::local(..))`, as `run_inner` does (mod.rs:2100-2235). The `startup_timing` `tui_first_frame` stage is kept.
4. Builds the agent off the input loop with `config.build_tui(vm)` or `build_resumed_tui(session, vm)` (config.rs:593-603). It emits `AgentConnecting` and then `AgentReconnected`. A failure shows `NotifyError` and offers a retry.
5. Replays history when resuming: either `TranscriptRecord::from_agent` over the rollout's events, or the session snapshot projection, then `AppEvent::HistoryReplayed`.
6. Starts the tui-control server as `"native"` with methods prompt/steer/cancel/models.list/history.list/history.read/settings.set. history.read pages from the local rollout file; WP2 owns the paging port.
7. Submits `--prompt` (InitialPrompt) once the agent is ready (port of L/mod.rs:1544-1576).

### 1.3 Turn lifecycle (local)
- Submit: unchanged `start_submission`, but with no managed request id bookkeeping. A local-only branch skips `submitted_turns`, `local_managed_turns` and `unacknowledged_inputs` and emits `LocalEvent::UserSubmitted` directly.
- Stream: `ConfiguredAgent.events` (AgentEvents) feeds `local/events.rs`, which maps each event to a record and sends `AppEvent::Transcript`. Records carry pane routing by agent id: main pane, /btw pane, or subagent node.
- Completion: `turn.result()` goes into the existing `Completion` JoinSet. It then emits `WorkerTurnFinished { terminal_expected: true }`.
- Cancel (Esc Esc) and steer/withdraw use `CancelTarget::Local` / `SteerTarget::Local` and `TurnControl` (already present).
- Settings (/model, /thinking|/effort, /fast):
  - Local mode applies them through the handle settings API, i.e. what the legacy worker's `SetModel`/`SetThinking`/`SetFastMode` used. They never go to `client.set_settings`.
  - Before the first prompt, a model change that crosses harness (Codex<->Claude) rebuilds the backend, as in L/mod.rs:1140-1170. WP1 owns that.
- Model catalog: local mode builds `Vec<ManagedModel>` from the harness catalog, using `nanocodex::Model` plus Claude models when Claude auth exists, instead of `client.models()`. The components already speak `ManagedModel` (components/model_selector.rs:12).

### 1.4 Effects in local mode: hide, replace, keep
Hidden means: not in the actions menu, not in `/` completion, not in the keybindings help. Typing the command anyway shows a clear `NotifyError`. All of this is driven by `Capabilities`, so it lives in one place.

| Managed feature (effect / command) | Local mode | Why |
|---|---|---|
| /share (+ShareOutput), /sites (+SitesOutput) | HIDE | client.create_share_link / list_sites (mod.rs:4246-4310) |
| /vault, ApproveVault, VaultReview | HIDE | managed vault |
| /secure-input, sudo input / private input | HIDE | client.private_input_browser_url (mod.rs:5383) |
| /screen | HIDE | needs a Hand via client (screen.rs) |
| /zoom | KEEP | pane layout only; useful with local /btw panes |
| /autoroute, RoutingHydrated | HIDE | client.routing_status (mod.rs:1624) |
| /done, /undone | HIDE | managed session list state |
| /connectors | HIDE | managed connectors |
| /bug | HIDE | posts through client (mod.rs:4903) |
| Handoff, Review download (LoadReviewBranches/ReviewDownload) | HIDE | managed fork/branches |
| /review (code review, review.rs has no client) | KEEP if it only submits a prompt; WP0 verifies, else HIDE | |
| /reload (binary hot reload) | HIDE | a non-durable session would be lost; managed reattaches by id |
| /attach, Sessions picker, SearchSessions, ResumeSession, NewSession(/clear) | REPLACE | local session source: native_sessions + rollouts (WP2). /clear rebuilds a fresh local agent |
| /btw, /close (managed client.fork, btw.rs:75) | REPLACE | local fork via handle.fork_from(snapshot)/spawn() (legacy L/mod.rs:2634-2726) (WP2) |
| /voice (ElevenLabs + managed voice protocol), voice clone | REPLACE | OpenAI Realtime when ConfiguredAgent.realtime is Some (WP4). Managed voice stays managed-only |
| Offline draft restore, recovery/reconnect, cancellation fences, steer receipts | N/A | durable-only machinery; skip by Backend::Local guards |
| Update banner, theme, keybindings, !, @, skills, Ctrl+R, Ctrl+G, Ctrl+V, Ctrl+O, subagents tree, context diagnostics, /copy, /goal, /id | KEEP | backend-neutral (/id shows the local session id) |

Legacy-only features, after porting:
- KEEP in both modes: tool-call Hidden mode and the flag, LaTeX, telemetry, notifications, `--prompt` (managed bare also accepts `--prompt`).
- LOCAL-ONLY at first: Claude interactions, scheduler, /mcp, /collapse, /split, branch navigator, eval/benchmark, Realtime voice.
- In managed mode these commands are hidden by `Capabilities::MANAGED`. A capability can later be enabled for managed once a managed implementation exists (for example `/split` → `nanocodex attach <btw-agent>` in a tmux/zellij pane).

---------------------------------------------------------------------------------------------------
## 2. Feature port map (target file <- legacy source)

| # | Feature | New/changed N2 files (owner WP) | Port from (legacy) |
|---|---|---|---|
| F1 | Local driver, capabilities, registry, --prompt, VM/MPP/browser/MCP lifecycle, /fast, /cancel | backend.rs, local/{mod,agent,events,updates}.rs, features/mod.rs, commands.rs, mod.rs (rename), root/app/composer/keybindings/actions.rs (registry hookup) (WP0) | L/mod.rs:807-1302 (run_observed), 1310-1350 (shutdown_runtime), 1544-1576 (initial prompt), 1794-1846 (spawn_agent_worker), 3006-3300 (start/steer/cancel_turn); L/startup.rs:150-242 |
| F2 | Claude host interactions: AskUserQuestion, ExitPlanMode/plan approval, permission ask | features/claude_interaction.rs, components/interaction.rs (new overlay) (WP1) | L/interaction.rs:1-67, L/mod.rs:1073-1088 (receive), 3896-3950 (respond), producer config/claude/interaction.rs:21-310 |
| F3 | Claude SessionScheduler pump (cron/loop/monitor) | features/claude_scheduler.rs (WP1) | L/mod.rs:1034-1070 (cron_tick + take_due), 1100-1110 |
| F4 | Harness selection --claude/--harness, claude_* flags, harness switch on /model before first prompt | local/agent.rs rebuild hook, features/harness.rs (WP1) | config.rs:172-266 (flags stay in AgentArgs), L/mod.rs:1140-1170, 3589-3648 |
| F5 | Local resume picker + ncl resume [ID] --from ROLLOUT --at N | local/sessions.rs (LocalSessionSource feeding components/session_picker.rs) (WP2) | L/resume_picker.rs (282), main.rs:373-488, rollout_fork.rs, native_sessions.rs |
| F6 | Rewind checkpoints | no TUI surface; ncl rewind stays CLI (main.rs:175-188); journey check rewind -> ncl resume (WP2) | rewind.rs |
| F7 | Branch navigator (Ctrl+Alt+B), historical prompt edit, fork at earlier turn | features/branches.rs, components/branch_navigator.rs (new overlay) (WP2) | L/mod.rs:2806-2930 (edit_historical, switch_main_branch), 3721-3864; L/app.rs:334 (fork_before), 1282-1480; L/view.rs:423, 631 |
| F8 | Local /btw fork, /collapse, /split | features/btw_local.rs, features/split.rs (WP2) | L/mod.rs:2481-2800 (collapse/open/split_btw), 4336-4408; L/split.rs (822, tmux/zellij) |
| F9 | tui-control "native" incl. history.read paging from rollout | local/control.rs (WP2) | L/control.rs:1-349 |
| F10 | Three-state tool display incl. Hidden + --tool-calls / NANOCODEX_TOOL_CALLS, Ctrl+O cycles | components/transcript/{mod.rs,tool.rs}, N2/tool_calls.rs (ToolCalls moves here from L/mod.rs:83-100) (WP3) | L/mod.rs:83-100, L/transcript.rs:70-100, 1286, L/view.rs:664 |
| F11 | LaTeX math (ratatex) | components/transcript/markdown.rs + new components/transcript/math.rs; one init helper math::init(&terminal) called by run_local and run_inner (WP3) | L/markdown.rs:1-200, L/startup.rs:226-242, L/mod.rs:1450 (flush_math_commands), L/app.rs:1550, L/terminal_profile.rs |
| F12 | Stream/view telemetry (keep), NO /trace, NO Jaeger | N2/telemetry.rs (new), hooked in local/events.rs and the render path (WP3) | L/telemetry.rs (664); L/mod.rs:852-1030 call sites. Do NOT port L/mod.rs:111-112, 4409-4441 |
| F13 | Desktop notifications parity | WP3 diffs L/notification.rs against the N2 notifier; port only what is missing | L/notification.rs |
| F14 | OpenAI Realtime voice, --voice-mute-key, --voice-animations | features/realtime_voice.rs, N2/voice_keys.rs (validate_key) (WP4) | L/voice.rs (336), L/mod.rs:1847-1900, config.rs:156-162 |
| F15 | /mcp login, /mcp reload | features/mcp.rs (WP4) | L/mod.rs:2155-2200, L/slash_commands.rs:47-55 |
| F16 | eval attach + /benchmark | move L/eval_attach.rs to src/eval/attach_view.rs (it is a standalone ratatui screen); features/benchmark.rs (WP4) | L/eval_attach.rs (355), eval/attach.rs:31, eval/benchmark.rs:112-115 |
| F17 | VM-backed local agents (VmArgs), MPP adapter, local browser config | lifecycle in local/agent.rs (WP0); journeys verified in WP4 | L/mod.rs:822-1000, 1310-1350 |

---------------------------------------------------------------------------------------------------
## 3. Hotspots and how to avoid conflicts

| File | Why hot | Rule |
|---|---|---|
| N2/mod.rs (7012) | run_inner, apply_update, DriverRuntime | WP0 only. Afterwards a WP may add exactly ONE line at an anchor that WP0 reserved, `// FEATURE-HOOK: <wp>` |
| components/root.rs (7168) | RootEffect, key routing, overlays, slash dispatch | WP0 adds RootEffect::Feature(FeatureEffect), Overlay::Feature(Box<dyn FeatureOverlay>), set_capabilities, and features::key_hook(&KeyEvent,&RootView)->Option<FeatureEffect> called before default key handling. No other WP edits root.rs |
| components/app.rs | AppEvent/AppEffect | WP0 adds AppEvent::Feature { pane, event: FeatureEvent } only |
| components/composer.rs | SettingsCommand::parse, take_local_command | WP0 replaces the match with commands::parse(input, caps); new commands become SettingsCommand::Feature(FeatureCommand) |
| components/keybindings.rs | fixed [(&str,&str); 39] | WP0 renders help from commands::help_rows(caps) (a slice) |
| components/actions.rs | Action enum + names | WP0 adds Action::Feature(FeatureCommand) rows from the registry |
| transcript/record.rs, transcript/model.rs | LocalEvent variants | WP0 pre-declares InteractionRequested{id,prompt,question}, InteractionResolved{id,answer}, ScheduledPromptFired{kind,label}, BranchSwitched{from,to}, BtwCollapsed{thread}, McpStatus{server,message}, VoiceCaption{..} with default rendering; WPs refine rendering only in their own component files |
| N2/features/mod.rs | dispatch | WP0 writes full enums + match; each arm calls <feature>::handle(..) in a file owned by exactly one WP |
| N2/local/updates.rs | LocalUpdate enum | pre-declared by WP0; features only send into it |
| main.rs / Stage A local tree, config.rs AgentArgs | entry + flags | WP0 only (type moves: ToolCalls -> N2/tool_calls.rs, validate_key -> N2/voice_keys.rs, PendingInteraction owned by config/claude/interaction.rs). Coordinate with agent 7 |
| components/transcript/* | rendering core | WP3 only |
| Cargo.toml | deps (ratatex already a nanocodex-bin dep) | WP0 makes the one dependency edit |

Slash-command registry (`N2/commands.rs`, owned by WP0):

```rust
pub(crate) struct CommandSpec {
    pub name: &'static str, pub aliases: &'static [&'static str], pub usage: &'static str,
    pub help: &'static str, pub needs: Capability,
    pub parse: fn(&str) -> Result<SettingsCommand, String>,
    pub action: Option<&'static str>, // actions-menu label
}
pub(crate) static COMMANDS: &[CommandSpec] = &[ /* one line per command */ ];
```

- WP0 adds every new command up front: /fast /cancel "/mcp login" "/mcp reload" /benchmark /collapse /split /branches. Each parses to `SettingsCommand::Feature(..)`, so feature WPs never edit the table.
- No `/trace`.

Ordering:
1. Stage A (agent 7): module merge, argv0 dispatch.
2. WP0 (serial, one agent): every hotspot edit, plus stubs. Merge it first. Gate: managed journeys stay green (tests/nanocodex2_tui_lifecycle.rs, shared-attach/sites/thread-share .mjs) and the WP0 local journeys pass.
3. WP1, WP2, WP3 and WP4 run in parallel with disjoint new files. Each rewrites its own legacy journeys against `ncl`.
4. WP5 (serial): delete `src/tui`, update consumers, docs and CI, remove Jaeger.

---------------------------------------------------------------------------------------------------
## 4. Work packages

Evidence for every package goes under output/unify/<wp>/: command, input, expected/observed result, and PTY transcript or screenshots.

### WP0 Foundation: local driver + extension points (serial, blocks all)
- Features: F1, F17 lifecycle, /fast and /cancel. Also the backend rename in mod.rs, Capabilities, the command registry, the Feature* enums, overlay/key hooks, pre-declared LocalEvents, and the type moves out of crate::tui. ncl bare/resume/benchmark call run_local; the legacy TUI still compiles but is unreachable.
- Write scope:
  - New: N2/{backend,commands,tool_calls,voice_keys}.rs, N2/local/{mod,agent,events,updates}.rs, N2/features/mod.rs + stub files (claude_interaction, claude_scheduler, harness, branches, btw_local, split, mcp, realtime_voice, benchmark).
  - Edited: N2/mod.rs, components/{root,app,composer,keybindings,actions}.rs, transcript/{record,model}.rs (enum additions only), main.rs local entry (with Stage A), config.rs (type paths), config/claude/interaction.rs.
- Acceptance (PTY via portable_pty or tui-control, with a stub Responses endpoint as in tests/harness_routing.rs):
  1. `ncl` in an empty tmp dir with NO account file paints the composer within the first frame. Then "hello" + Enter streams the stub reply, and the footer returns to idle.
  2. `ncl --prompt "say hi"` submits automatically once.
  3. During a long stubbed turn: Esc Esc cancels it. A queued message plus Alt+U withdraws it. A steer is admitted.
  4. /share, /sites, /vault, /screen, /autoroute, /done and /bug each show the local "needs an account session" error. None of them appear in / completion, the actions menu or the keybindings help.
  5. The /model picker lists local models and the switch persists for the next turn. /thinking high and /fast on apply.
  6. !echo hi runs, @ finds a file, Ctrl+G opens $EDITOR, Ctrl+V pastes an image, and Ctrl+C Ctrl+C exits with the terminal restored.
  7. `nanocodex tui list` shows the session as kind native, and `nanocodex tui prompt` drives it.
  8. Managed regression: nanocodex2_tui_lifecycle and the three managed .mjs journeys pass unchanged.
  9. Where the hardware allows, `ncl --vm ...` starts a VM agent and exits without an orphaned VM. Otherwise this is a documented gap.

### WP1 Claude host + harness
- Features: F2, F3, F4.
- Write scope:
  - N2/features/{claude_interaction,claude_scheduler,harness}.rs and components/interaction.rs (new).
  - The WP1 hook function in local/agent.rs.
  - Rewrites of scripts/tests/claude-{interaction,permissions,model-picker,code-mode,loop,monitor-ws,scheduler-monitor,tui-image,native,lifecycle}-cli-journey.py and the wrappers tests/claude_interaction.rs and claude_scheduler_monitor.rs, so they launch `ncl`.
- Depends on: WP0.
- Acceptance (`ncl --claude` with the existing Claude stub fixtures):
  1. AskUserQuestion shows an overlay with the options. Picking option 2 sends that answer in the tool result.
  2. `--permission-mode plan`: ExitPlanMode shows the plan. Approve continues the turn; reject with feedback returns the feedback to the model.
  3. `--claude-permissions ask`: a Bash tool permission prompt is accepted once and denied once, and both outcomes are visible.
  4. /loop (or a cron tool) fires a scheduled prompt while idle, and a monitor event triggers a turn. Neither fires while an interaction is open.
  5. Before the first prompt, switching /model between Codex and Claude rebuilds the backend. The stub request log shows the new harness on the next turn.
  6. The claude-tui-image journey passes.

### WP2 Sessions, branches, side threads, control history
- Features: F5, F6 (journey only), F7, F8, F9.
- Write scope:
  - N2/local/{sessions,control}.rs, N2/features/{branches,btw_local,split}.rs and components/branch_navigator.rs (new).
  - Rewrites of tests/{codex_resume_picker,codex_resume_from,claude_resume,tui_control}.rs and scripts/tests/{codex-resume-picker,codex-resume-from,claude-resume,claude-conversation-rewind}-*.py, so they target `ncl`.
  - A PTY journey that replaces tests/tui_split.rs.
- Depends on: WP0. WP2 implements LocalSessionSource against the session-source trait WP0 defines; it never edits session_picker.rs.
- Acceptance:
  1. `ncl resume` lists recent local Codex and Claude sessions. Picking one replays its history, and the next prompt continues it.
  2. `ncl resume --from <rollout> --at 2` opens a fork whose transcript stops at turn 2.
  3. /attach inside ncl opens the same local picker.
  4. Ctrl+Alt+B opens the branch navigator. Editing turn 1's prompt creates a new branch; switching back shows the original transcript.
  5. /btw opens a side pane forked from the main snapshot. /collapse merges a summary into main. /split inside tmux opens a pane running `ncl resume <btw>`. /close closes the pane.
  6. tui-control history.read pages backwards through the rollout with stable cursors.
  7. After `ncl rewind <s> --checkpoint N --restore`, `ncl resume <s>` shows the rewound transcript.

### WP3 Rendering + telemetry
- Features: F10, F11, F12, F13.
- Write scope:
  - components/transcript/* and N2/{tool_calls,telemetry}.rs bodies.
  - N2 notification parity edits.
  - Rewrites of tests/tui_log_location.rs + scripts/tests/tui-log-location-journey.py.
  - Math and hidden-tool cases in the N2 bench (N2/bench.rs), replacing benches/tui_render.rs.
- Depends on: WP0.
- Acceptance:
  1. `ncl --tool-calls hidden` (and NANOCODEX_TOOL_CALLS=hidden) shows no tool rows while the footer shows Working. Ctrl+O cycles Expanded -> Folded -> Hidden.
  2. Managed `nanocodex` honours the env var the same way.
  3. Under a kitty-graphics terminal, a reply with display math renders an image. Otherwise it falls back to Unicode text. Screenshot evidence.
  4. With OTLP sent to a local collector file sink (no Jaeger), a turn emits stream/view telemetry spans with the session id. /trace is an unknown command.
  5. The log-location journey passes for ncl.

### WP4 Voice, MCP, eval, platform agents
- Features: F14, F15, F16, and F17 journeys.
- Write scope:
  - N2/features/{realtime_voice,mcp,benchmark}.rs and N2/voice_keys.rs body.
  - src/eval/{attach_view (moved from L/eval_attach.rs), attach, benchmark}.rs.
  - Rewrites of scripts/tests/computer-classic-tui-journey.py and the TUI parts of claude-mcp-cli-journey.py (and tests/it/mcp_cli.rs if it has any).
- Depends on: WP0.
- Acceptance:
  1. /voice on starts Realtime and /voice list lists voices. `--voice-mute-key ctrl+y` toggles mute and the footer shows it. `--voice-animations false` disables caption animation. An invalid key fails at parse time.
  2. /mcp login <server> completes against a stub OAuth MCP server, and /mcp reload <server> shows the reloaded tool count.
  3. `ncl eval attach <run>` shows the live table, and /benchmark smoke starts a run.
  4. With feature tempo, a stub paid tool call through MPP succeeds, and the browser config is honoured.
  5. The computer-classic-tui journey passes on ncl.

### WP5 Deletion, consumers, docs, Jaeger removal (serial, last)
- Write scope: the section 5 checklist.
- Depends on: WP1-WP4 merged.
- Acceptance:
  - `git grep "crate::tui::"` hits only the N2 TUI.
  - `git grep -i jaeger` returns nothing outside CHANGELOG history.
  - ci.yml nextest and check-fast.mjs pass.
  - All rewritten journeys pass with the single binary invoked both as `ncl` and as `nanocodex --local`.

---------------------------------------------------------------------------------------------------
## 5. Deletion checklist (bin/nanocodex/src/tui/)

Code:
- main.rs:
  - Drop `mod tui;`.
  - Bare local, resume and --prompt call run_local (old 418-498).
  - tui::select_resume_session moves to WP2's picker.
- config.rs:157 and 169: change the validate_key and ToolCalls paths.
- config/claude/interaction.rs:23: it now owns PendingInteraction.
- eval/attach.rs:31 -> eval::attach_view.
- eval/benchmark.rs:112-115 -> run_local.
- observability.rs: keep OTLP export and drop any trace-UI URL helpers.
- Delete the legacy Jaeger code (L/mod.rs:111-112 DEFAULT_JAEGER_UI_URL/NANOCODEX_JAEGER_UI_URL, 4409-4441 session_trace_url/open_session_traces) along with the directory.

Tests and benches:
- Delete benches/tui_render.rs and its [[bench]] (Cargo.toml:110-112). Drop `--bench tui_render` in scripts/check-fast.mjs:60.
- Delete tests/tui_split.rs and its [[test]] (Cargo.toml:114-116).
- Rewrite to launch `ncl` (owners listed per WP):
  - tests/tui_control.rs
  - the TUI cases in tests/harness_routing.rs
  - tests/tui_log_location.rs
  - codex_resume_picker.rs, codex_resume_from.rs, claude_interaction.rs, claude_resume.rs, claude_scheduler_monitor.rs
  - the scripts/tests PTY journeys: claude-*, codex-resume-*, tui-log-location, computer-classic-tui
- Headless `ncl run` tests (claude_workflow.rs, durable_run.rs, ...) are unaffected.
- tests/it/observability_stress.rs uses the Jaeger query API (NANOCODEX_STRESS_JAEGER_URL). Rewrite it against an OTLP collector file sink, or delete it with a documented gap.
- About 215 legacy in-module unit tests go with the code. Do not port helper tests (AGENTS.md).
- The N2 bench path changes with Stage A.

CI and scripts:
- ci.yml:313-320: keep nanocodex-bin build/nextest.
- Add the hermetic WP journeys to scripts/ci/select-jobs.mjs.
- check-fast.mjs:60.

Docs:
- README.md:363-379 (nanocodex --claude -> ncl --claude) and README.md:761 (tui-control kinds).
- docs/RICH_TERMINAL_INTEGRATION.md:11-12: native = ncl, managed = nanocodex.
- docs/CLAUDE_RUNTIME.md:7, 462-489; docs/TOOL_RUNTIMES.md:53; docs/VM.md:145.
- docs/OBSERVABILITY.md:
  - Remove the Jaeger and /trace sections (around 81, 109, 230-250).
  - Document an OTLP collector (file or debug exporter) instead.
- docker-compose.otel.yml: remove the jaeger service. Either keep a collector-only compose file or delete the file and its doc references.
- CHANGELOG.md:
  - The legacy TUI was removed; ncl/--local open the unified TUI.
  - /trace and the Jaeger compose service were removed.
  - Ctrl+O cycles three states in both modes.
  - Moved commands: ncl resume, ncl run, ncl auth.

---------------------------------------------------------------------------------------------------
## 6. Risks

1. **Hot-file churn.** The client->backend rename touches about 40 sites in the 7k-LOC mod.rs while agent 7 moves the tree. Mitigation: start WP0 only after Stage A lands. Later WPs never edit mod.rs or root.rs except at the reserved hook lines.
2. **Durable-only invariants vs local turns.** unacknowledged_inputs, cancellation fences, steer receipts, recovery and offline drafts all assume a ManagedEvent echo for every submission. Guard each with Backend::Local; the managed regression journeys are the gate.
3. **Event-model mismatch.** N2 rendering was tuned on the ManagedEvent projection (history.rs). AgentEvent-only streams may group tool deltas and reasoning differently, and subagent and /btw pane routing differ too. Mitigation: a WP0 screenshot comparison of local vs managed rendering of the same stub turn.
4. **Claude interaction UX.** Legacy answers through the composer (L/mod.rs:3902). N2 needs a modal that must not race the queue/steer editor. Scheduler fires are suppressed while an interaction is open (L/mod.rs:1038).
5. **Branch navigator size.** Legacy keeps per-branch transcripts (main_branches); N2's TranscriptModel is a single timeline, so each branch switch needs a projection swap. This is the largest UI port (about 1k LOC). Keep it local-only.
6. **Two voice stacks.** Managed uses ElevenLabs plus the voice protocol; local uses OpenAI Realtime. Both use Ctrl+X mute and /voice. Gate divergent subcommands (clone vs list) by capability.
7. **Graphics escapes.** ratatex/kitty graphics and the N2 image renderer (components::initialize_image_renderer, screen_graphics.rs) both emit them. Coordinate image placement IDs.
8. **Keybinding collisions.** Audit legacy Ctrl+Alt+B, three-state Ctrl+O and the configurable voice mute key against N2's keybindings before WP0 freezes the registry.
9. **tui-control native contract.** Rich-terminal clients rely on L/control.rs history.read cursors. Port the format exactly.
10. **Silent behaviour loss.** About 215 legacy unit tests disappear. Each WP must record any legacy behaviour without a journey as an explicit gap before WP5 deletes the code.
11. **Startup latency.** build_tui (MCP servers, VM boot, Claude auth) must run after the first frame, off the input loop. Watch the tui_first_frame timing stage.
12. **cfg gates.** VM/eval exist only on Linux glibc and macOS aarch64, and MPP only under feature tempo. run_local must mirror the config.rs cfgs. Build both with default features and with --all-features.
