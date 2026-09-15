**Swift and web UI rebuild — 9 September 2026**

The user wants to apply the Superlogical/Hashimoto approach across existing Swift UIs, especially desktop. The proposed standard is responsive native software: explicit ownership and scheduling, measured launch and interaction costs, coordinated animation, bounded resource use, and platform-appropriate materials and behavior.

This document combines public engineering evidence with proposed requirements for our applications. The requirements are our synthesis, not a published Superlogical style guide. The continuation applied the findings to Nanocodex desktop, mobile, shared Swift UI, and web surfaces; implementation evidence is recorded below.

**Research coverage**

Reviewed 215 profile posts through BrowseX across Mitchell Hashimoto, Alasdair Monk, and Hector Simpson, plus selected conversations and Ghostty PRs. The recent material covers Superlogical's launch through September 9, with earlier posts and GitHub history supplying technical context. X search repeatedly returned an upstream 503; profile pagination and individual post reads worked. This is a substantial source-backed survey, not an exhaustive archive. The original hosted turn assessed video-linked posts from accompanying text. The continuation downloaded all 22 entries in its recovered video manifest, inspected timestamped contact sheets throughout each clip, and read automatic transcripts of all six audio-bearing clips. Silent demos were sampled every second; longer clips every ten seconds. This is sampled visual review, not inspection of every source frame. Transcription errors and trailing hallucinations were excluded from the findings. Performance figures below are the authors' reported measurements, not reproduced benchmarks.

**Evidence worth carrying into implementation**

1. **Measure the real launch path, including framework side effects.** Mitchell's August 10 startup work reports process execution to visible window improving from about 193 to 165 ms, and initialization to the first GPU-completed frame from about 126 to 92 ms. Those are distinct measurements: rendering can finish before AppKit presents the window. He deferred keyboard-map initialization, moved Sentry directory resolution onto its initialization thread, warmed thread-safe CoreText/Metal machinery concurrently, performed exact font lookup, and cached logging objects. The commits also avoid loading a configuration-error window when there are no errors and avoid materializing native tab-group machinery unnecessarily. [Merged PR #13722](https://github.com/ghostty-org/ghostty/pull/13722).

   Application: trace launch from entry through visible content and first usable interaction. Inspect work done before a background task starts, property getters that initialize framework machinery, hidden controllers, and unnecessary setup. Each optimization needs behavior checks: the startup PR subsequently required a window-cascading correction. [Follow-up #14106](https://github.com/ghostty-org/ghostty/pull/14106).

2. **Represent UI ownership explicitly; moving work to another thread also requires a data-access design.** Ghostty made SearchState MainActor-isolated when integrating NSPasteboard because that state belongs to the main thread. An earlier terminal-search proposal was rejected with unsafe concurrent PageList access among the unresolved issues. A later fine-grained synchronization proposal was closed after a simpler shared-lock architecture shipped. Mitchell specifically requested before/after terminal-stream benchmarks with search disabled. [Merged #12712](https://github.com/ghostty-org/ghostty/pull/12712), [unmerged #8516](https://github.com/ghostty-org/ghostty/pull/8516), [unmerged #8850](https://github.com/ghostty-org/ghostty/pull/8850), [alternative #9585](https://github.com/ghostty-org/ghostty/pull/9585).

   Application: UI state has an explicit actor owner; workers have separate ownership or deliberately scoped synchronization. Measure the ordinary path when adding optional concurrency-heavy features. Prefer the least complicated synchronization that meets correctness and latency needs.

3. **Keep blocking shutdown operations outside shared drawing locks.** On September 7, Mitchell fixed a deadlock caused by stopping CVDisplayLink while holding the drawing mutex. Stopping the display link blocks on a CoreVideo thread; the main-thread Core Animation display callback also acquires the drawing mutex. [Merged #14171](https://github.com/ghostty-org/ghostty/pull/14171).

   Application: document lock ordering and callback ownership around renderers, timers, display links, and shutdown. Inspect indirect waits and re-entrant framework calls, not just explicit calls to main.sync.

4. **Asynchronous teardown can violate object lifetime.** A contributor's July fix frees a terminal surface synchronously when deinit already runs on the main thread, preventing delayed scrollbar callbacks from reaching dead view state. Reviewers emphasized the lifecycle contract and retained reservations about the coupling. This is a narrow correctness fix, not a general recommendation to do expensive destruction on the UI thread. [Merged #13364](https://github.com/ghostty-org/ghostty/pull/13364).

   Application: detach callbacks and cancel owned work in a defined sequence; ensure callback targets stay alive for their entire permitted lifetime. Do not scatter detached tasks through teardown to silence isolation problems.

5. **Use SwiftUI and lower-level native UI together where their strengths fit.** On June 25, Mitchell described returning to NSView/Core Animation for custom animation requiring precise frame coordination. On June 29, he described accepting caller-provided SwiftUI focus views but hosting them through AppKit/UIKit so their Core Animation movement stays synchronized with the rest of the split UI. Accessibility support was part of the framework work. [Initial observation](https://x.com/mitchellh/status/2070262735896772843), [implementation update](https://x.com/mitchellh/status/2071657456854605869).

   Application: keep a convenient SwiftUI composition API. Use native controllers, views, and layers for interactions that need more control, backed by measurements and visible behavior. This is not evidence that every SwiftUI view needs replacement.

6. **Separate logical layout from the view hierarchy.** Mitchell keeps the split model as a tree but resolves it into a flat two-dimensional arrangement of panes. New panes begin at their final size and animate into view. A common overlay handles drop targets across divider boundaries; masks prevent panes showing through translucent neighbors during entry. [June 25 implementation details](https://x.com/mitchellh/status/2070281069631611056).

   Application: for pane workspaces, compute stable pane rectangles from the model, preserve content identity, and coordinate movement within one layout surface. Use position, clipping, and layer presentation where appropriate instead of repeatedly resizing expensive live content. Genuine user resize still needs correct layout and content updates.

7. **Preserve the user's content and attention during navigation.** Superlogical's tab peek moves the terminal without resizing it, avoiding reflow. Alasdair's search UI floats above the terminal to avoid reflow, then moves out of the way of the active match so it does not hide the result. [Tab peek](https://x.com/mitchellh/status/2087537750182666290), [search UI](https://x.com/almonk/status/2095134190631096428).

   Application: overlays must preserve scroll position, selection, keyboard focus, and visibility of the thing the user is inspecting. Apply this to terminals, code editors, logs, tables, and other expensive or position-sensitive content.

8. **Treat hidden-window resource use as a separate workload.** Mitchell's August 25 post reports roughly 10x less memory for a visible window and 450x less for a non-visible window in his Ghostty comparison. Releasing most GPU resources when windows were fully occluded was a major contributor; the charts excluded scrollback and used closely matched grids. His July compression work separately reports 70–90% less physical scrollback memory. These are workload-specific results, not savings to expect automatically in our applications. [GPU/resource post](https://x.com/mitchellh/status/2092326409071198210), [scrollback compression](https://x.com/mitchellh/status/2075284760583418284).

   Application: distinguish visible, occluded, minimized, inactive-tab, and closed states. Stop unnecessary rendering and subscriptions; release expensive recreatable resources under an explicit policy. Preserve the model and keep the path back to visibility fast. Measure both steady-state savings and restoration latency.

9. **Small visual effects can dominate cost.** A contributor optimized Ghostty's secure-input inner-shadow animation, reporting about 35% lower CPU with comparable appearance and fewer rendering problems. Mitchell approved and merged it. Separately, Mitchell reported repeated warnings consuming about 10% of terminal IO time in a workload before a log-once helper removed that overhead. [Animation #10903](https://github.com/ghostty-org/ghostty/pull/10903), [logging post](https://x.com/mitchellh/status/2078175337922621693).

   Application: profile shadows, masks, effects, animation updates, and logging in the actual interaction. Consolidate repeated work and avoid rebuilding expensive resources on each update. Visual simplicity alone does not prove low cost.

10. **Liquid Glass needs coherent compositing and deliberate customization.** Alasdair's September 8 post shows a range from full glass to solid system palettes, 41 themes tinting the application, and comfortable/compact density. In Ghostty, February's inactive-window tint work was revised in August to preserve the background color and composite glass above it, simplifying behavior and fixing opacity issues. [Superlogical appearance](https://x.com/almonk/status/2097439320076403125), [initial #10943](https://github.com/ghostty-org/ghostty/pull/10943), [later merged #13928](https://github.com/ghostty-org/ghostty/pull/13928).

   Application: define semantic appearance roles for content, chrome, overlays, focused/inactive surfaces, and selection. Keep material and base color responsibilities clear. Validate translucency over busy backgrounds and across window focus states. Adopt density and appearance options when they serve the actual product rather than copying a fixed theme count.

11. **Old availability snippets are historical context.** Mitchell's June 2025 glass helper checked both runtime OS and the build SDK; it explicitly omitted the compatibility opt-out key. Ghostty later removed the helper when its build process guaranteed the relevant SDK. [SDK issue #7591](https://github.com/ghostty-org/ghostty/issues/7591), [glass.swift gist](https://gist.github.com/mitchellh/351c37999031a82119cb50b41dc755ba), [build change #7616](https://github.com/ghostty-org/ghostty/pull/7616).

   Application: inspect the actual deployment target, SDK, compatibility configuration, and supported OS versions before adopting material code. Do not paste the historical helper as a complete modern capability or accessibility check.

12. **Optimize on the weakest supported hardware.** Mitchell bought a 2020 Intel MacBook Air with an i3 and 8 GB RAM specifically to optimize Superlogical on its worst supported hardware. His startup discussions also emphasize the visible Dock bounce as a useful experiential signal. [Hardware post](https://x.com/mitchellh/status/2094494569857777750), [startup post](https://x.com/mitchellh/status/2093700021166485893).

   Application: choose a realistic low-end device from our own support matrix and keep it in the performance loop. Use numerical traces alongside visual inspection; a Dock bounce is not a stable scientific timing unit.

**Swift and Apple documentation that qualifies the recommendations**

These rules come from Apple/Swift sources, not statements attributed to the Superlogical team.

- A Task created in a MainActor-isolated context inherits that isolation. Wrapping synchronous expensive work in Task can postpone the same UI hang. async/await does not itself promise execution away from the main actor. [Apple responsiveness guidance](https://developer.apple.com/documentation/xcode/improving-app-responsiveness).
- Swift 6.2 implements SE-0461. With NonisolatedNonsendingByDefault enabled, plain nonisolated async functions remain on the caller's actor; @concurrent explicitly moves an async function off the caller's actor and requires appropriate cross-isolation transfers. Without that feature, the older plain-nonisolated-async behavior remains. Audit build settings before choosing the pattern. [SE-0461](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0461-async-function-isolation.md).
- Swift's cooperative pool depends on forward progress. Avoid semaphore waits that block a worker pending future task work. Short correctly scoped critical sections are not categorically forbidden, and Task.detached does not provide a dedicated thread for blocking IO. [Swift concurrency: Behind the scenes](https://developer.apple.com/videos/play/wwdc2021/10254/).
- Use the SwiftUI Instruments template to distinguish expensive body calculations from too many updates. Keep view initialization and update callbacks small and reduce unnecessary state dependencies. [SwiftUI performance](https://developer.apple.com/documentation/xcode/understanding-and-improving-swiftui-performance).
- Prefer standard controls, use custom glass sparingly, and group related custom glass effects with GlassEffectContainer. Standard components adapt to accessibility settings; test custom materials and animations under Reduce Transparency and Reduce Motion. [Adopting Liquid Glass](https://developer.apple.com/documentation/technologyoverviews/adopting-liquid-glass).

**Proposed architecture for our desktop applications**

| Area | Default ownership and approach | Acceptance evidence |
| --- | --- | --- |
| Domain and session state | Explicit owners independent of view lifetimes; workers return safe results | No races or stale results during rapid navigation and cancellation |
| UI state | MainActor-owned presentation state with narrow observation dependencies | Main-thread traces show short commits and bounded update work |
| Composition | SwiftUI for ordinary composition, controls, forms, and configuration | Native interaction and accessibility behavior preserved |
| Windowing and precise interaction | AppKit where focus, responder chain, menus, window lifecycle, or complex interaction needs control | Keyboard navigation, multiwindow behavior, resizing, and restoration exercised |
| Animation | Core Animation for measured coordination needs; one owner for each animated property | Inspect every frame of representative transitions, including interrupted transitions |
| Live content | Stable identity and geometry; rendering independent of decorative chrome | No unintended content reflow, selection loss, or surface recreation |
| High-throughput rendering | Retain or introduce Metal only where the workload warrants it | Measured CPU/GPU time and allocation behavior justify the implementation |
| Appearance | Semantic color/material roles, platform behavior, useful density choices | Light/dark, active/inactive, busy backgrounds, accessibility settings checked |
| Resource lifecycle | Explicit visibility, suspension, restoration, and teardown transitions | Repeated open/close and hide/show settle to stable resource use |

This is a proposed default, to be adapted after inspecting each codebase. It does not assume one shared renderer or one identical layout framework fits every application. On iOS, use UIKit where the corresponding lower-level integration is needed rather than trying to reuse AppKit internals.

**Migration sequence**

1. Inventory the selected application's windows, navigation, live content, view ownership, tasks, timers, renderers, deployment targets, and existing tests. Identify shared components only after observing actual duplication.
2. Capture a release-build baseline for cold/warm launch, first visible content, first usable interaction, typing, scrolling, tab switching, pane resizing, search, and window restore. Record hardware, OS, dataset, window/grid size, display refresh rate, and repeatability.
3. Repair ownership, cancellation, callback lifetime, and main-thread stalls. Separate necessary presentation work from IO, parsing, indexing, decoding, and expensive computation. Do not just add async keywords or detach tasks.
4. Rebuild one representative high-value interaction end to end, including native input, accessibility, animation, and resource lifecycle. A live workspace with tab switching and a search overlay is a strong candidate if the application has one.
5. Establish shared components from the successful implementation. Move remaining screens incrementally, keeping behavior and performance comparisons reviewable.
6. Add or refine material, theme, density, and animation polish once layout and lifecycle are sound. Re-profile after introducing effects.

**Proposed release checks**

Set numerical budgets after the baseline and support matrix are known; do not copy Ghostty's millisecond figures as promises for unrelated applications. At 60 Hz the full display interval is about 16.67 ms; at 120 Hz it is about 8.33 ms, shared by the entire rendering pipeline. Our own main-thread work should occupy only a portion of that interval.

| Workload | What to record or verify |
| --- | --- |
| Launch | Median and tail latency across repeated runs; visible content and interaction separately; deferred work does not cause a first-keystroke stall |
| Input and scroll | Response latency, frame pacing, long main-thread work, and SwiftUI update fan-out |
| Layout and animation | Resize correctness; interrupted/reversed transitions; preserved content identity, focus, selection, and scroll position |
| Idle and hidden | CPU wakeups, memory, GPU resources, active timers; restoration latency when visible again |
| Repeated lifecycle | Open/close, tab churn, window occlusion, reconnect, cancellation; resources settle after transient work |
| Concurrency | Ownership and transfer checks, focused race/deadlock validation, cancellation and stale-result behavior |
| Native behavior | IME/text input, keyboard-only use, responder chain, menus, Find pasteboard where appropriate, multiwindow behavior |
| Accessibility and appearance | VoiceOver, Reduce Motion, Reduce Transparency, light/dark, inactive windows, differing display scales |

Use targeted tests and repeatable interaction traces where they protect real behavior. Performance work must preserve native semantics; the window-cascading regression after Ghostty's startup change is a useful reminder to pair timing improvements with behavior verification.

**Additional team examples**

- In pearkes' clipboard PR, Mitchell changed the proposed public API from raw protocol fields to a semantic clipboard-write interface with normalized destinations and decoded MIME content. The opening PR description predates that revision; inspect the final commits. Application: give Swift UI clients a stable domain API instead of making them interpret terminal protocol details. [Merged #13182](https://github.com/ghostty-org/ghostty/pull/13182).
- Alasdair's scrollback/selection proposal emphasizes preserving position while output arrives. Mitchell recommended first shipping a simple clickable indicator that more output exists, avoiding exact line-count complexity. This remains an open proposal, not verified shipped behavior. Application: preserve interaction continuity and keep the first useful implementation small. [Issue #2001](https://github.com/ghostty-org/ghostty/issues/2001), [Mitchell's comment](https://github.com/ghostty-org/ghostty/issues/2001#issuecomment-2365340388).


**Continuation: implementation and scope**

Recovered hosted agent `01a08669-3851-7d44-af51-cacb3f44b9d8` and its worktree `nanocodex-ui-rebuild` on branch `ui/superlogical-rebuild-20260909`. The hosted turn repeatedly returned 503/timeouts while retaining an active marker. Cancellation was requested but not confirmed; no hosted completion is claimed. Its inherited dirty state is preserved by baseline tree `365310541f2b0e28b0fe994e4b39cfc125f26d61`. Compare against that tree to distinguish this continuation from concurrent/unrelated work.

The audit found substantial existing alignment: desktop already owns a flat AppKit pane surface with stable SwiftUI hosts, shared UI already has off-main generated-asset decoding and bounded markdown caches, and the main voice orb already respects scene activity. Reusing those boundaries produced focused fixes rather than replacing those implementations.

| Surface | Concrete change | Reason |
| --- | --- | --- |
| macOS tiled workspace | Set final host geometry once, then animate Core Animation presentation position/opacity; resume interrupted moves from presentation state | Avoid repeated live-content resizing during pane rearrangement |
| macOS visibility and direct manipulation | Skip structural animation in occluded windows; divider placement removes owned motion animations | Avoid invisible work and keep the divider directly attached to input |
| iPhone/iPad tabs | Semantic subheadline text and scaled, stable tab dimensions capped to the available width | Preserve lazy-strip targeting while honoring Dynamic Type |
| Mobile lifecycle | Clear tab-edge scrubbing and pause status pulse/spinner while scene is inactive | Stop decorative or gesture-owned work when the scene cannot be interacted with |
| Shared generated outputs | Pause generated media when inactive; reset file state and reject cancelled download results | Avoid hidden playback and stale downloadable files after navigation |
| Mobile materials | Opaque card fallback for custom header glass under Reduce Transparency | Keep the custom material's accessibility policy explicit |
| Web materials | Remove per-button blur; opaque reduced-transparency fallbacks on navigation, dialogs, overlays, and docs toolbar | Reduce repeated compositing and keep text readable |
| Web motion | Disable smooth scrolling under reduced motion | Honor the preference for navigation as well as animations |

No public JavaScript contract changed. Existing component boundaries, icons, native controls, and logical pane models were retained. Terminal protocol compression, SSH replacement, new theme catalogues, and decorative shutdown effects from the references are not requirements for these agent clients.

**Video evidence and application**

The local archive is `../nanocodex-ui-rebuild-evidence/videos/` relative to the worktree. It includes the recovered source manifest, 22 validated MP4s (44.5 minutes total), per-clip metadata, 41 timestamped contact sheets, and six automatic transcripts. Source metadata and research transcripts remain outside the repository. The table records original conclusions, not a redistribution of source transcripts. Timestamps are approximate.

| Source clip | Evidence inspected | Consequence for Nanocodex |
| --- | --- | --- |
| [mitchellh · 2097424868203758046](https://x.com/mitchellh/status/2097424868203758046) | Remote persistence, 0:59–1:22, 3:22–4:07, 4:49–5:42 | Preserve session identity across navigation; keep local and remote navigation coherent. |
| [mitchellh · 2095232081853039041](https://x.com/mitchellh/status/2095232081853039041) | Server memory discussion, 3:54–6:40, 6:43–9:10, 11:01–11:30 | Visibility and idleness need distinct lifecycle policies; terminal-specific savings are not Nanocodex benchmarks. |
| [mitchellh · 2093451043661316217](https://x.com/mitchellh/status/2093451043661316217) | Basic demo, 0:11–0:21, 0:55–1:36, 1:57–2:19 | Measure usable interaction and preserve sessions; retain native scrolling. |
| [mitchellh · 2087537750182666290](https://x.com/mitchellh/status/2087537750182666290) | Tab peek, entire silent clip | Move presentation without continuously changing live-content size. |
| [mitchellh · 2082936029426892960](https://x.com/mitchellh/status/2082936029426892960) | Architecture discussion, 2:07–3:21, 5:01–6:07, 6:53–8:06 | Separate authoritative session state from client viewport state; prioritize current content before history. |
| [mitchellh · 2079327969416482859](https://x.com/mitchellh/status/2079327969416482859) | Split focus, tab switching and rearrangement, entire clip | Stable identity and focus must survive coordinated movement. |
| [mitchellh · 2075284760583418284](https://x.com/mitchellh/status/2075284760583418284) | Compression demo, 0:15–1:45, 2:22–3:18 | Schedule optional work around interaction; do not introduce terminal memory machinery without matching workload evidence. |
| [mitchellh · 2071657456854605869](https://x.com/mitchellh/status/2071657456854605869) | Split manipulation, entire clip | Keep pane focus and corner/divider manipulation coordinated. |
| [mitchellh · 2070273858154987537](https://x.com/mitchellh/status/2070273858154987537) | Nested split additions/removals and layout changes, entire clip | Logical tree and flat hosting surface can remain separate. |
| [mitchellh · 2049851176988914071](https://x.com/mitchellh/status/2049851176988914071) | Terminal UI exercise and stalled-render indication, entire clip | Check real interaction and stalled rendering; a successful build alone is insufficient. |
| [almonk · 2097439320076403125](https://x.com/almonk/status/2097439320076403125) | Appearance settings, entire clip | Opaque/system variants need equal care alongside glass. |
| [almonk · 2095134190631096428](https://x.com/almonk/status/2095134190631096428) | Search strip, especially 0:01–0:03 | An overlay must move out of the way of the item being inspected without reflow. |
| [almonk · 2092908172381982782](https://x.com/almonk/status/2092908172381982782) | Horizontal/vertical navigation, entire clip | Preserve navigation structure and accessible labels rather than copying a layout preference. |
| [almonk · 2088311080476872898](https://x.com/almonk/status/2088311080476872898) | Theme changes, entire clip | Coordinate content and chrome colors; inspect light and dark rendering. |
| [almonk · 2087535957100613892](https://x.com/almonk/status/2087535957100613892) | Tab peek, entire clip; repeated view of the other tab-peek demonstration | Same stable-content principle; do not count repeated media as independent performance evidence. |
| [almonk · 2087533118429294920](https://x.com/almonk/status/2087533118429294920) | Session organization, 0:16–1:05; peek and overview, 1:44–3:12 | Preserve live content while changing the navigation view; expose grouping without losing the current session. |
| [almonk · 2084691808722751681](https://x.com/almonk/status/2084691808722751681) | 404 stack effect, entire clip | Decorative brand example; no corresponding product behavior needed. |
| [almonk · 2084549282120511575](https://x.com/almonk/status/2084549282120511575) | Tab identity and rearrangement, entire clip | Keep app/session identity visible across tab movement. |
| [almonk · 2082719115714707913](https://x.com/almonk/status/2082719115714707913) | Presskit fan on hover, entire clip | Decorative content reveal; not a replacement for explicit accessible controls. |
| [dizzyup · 2087543392859193368](https://x.com/dizzyup/status/2087543392859193368) | Icon studio, entire clip, including light/dark previews and package workflow | Retain a consistent existing icon system; validate size and appearance variants. |
| [mitchellh · 2071688415524049208](https://x.com/mitchellh/status/2071688415524049208) | Pane zoom/unzoom, entire clip | Check intermediate transitions and interruptions; a short demo does not prove artifact-free animation. |
| [almonk · 2097612610166288713](https://x.com/almonk/status/2097612610166288713) | Pixel-melt closing effect, entire clip | Brand flourish only; retain clear closure and lifecycle semantics. |


**Verification and reproducibility**

Evidence lives in the sibling `nanocodex-ui-rebuild-evidence` directory. Builds and runtime checks used the isolated worktree on macOS 26.3.1 / arm64, Xcode's iOS 26.5 simulator runtime, and the repository's pinned Node 24 / pnpm 11.25.0 through Corepack. Unrelated main-checkout work was not part of the build claims.

- macOS: `testNativeDockingRetainsEditorsAndRendersGlass`, `testNativeWorkspaceRenderingAndInteractionLatency`, and the new `testPaneMotionCommitsSizeOnceAndLiveResizeCancelsMotion` passed. The new regression checks immediate final content geometry and cancellation of movement during direct resize. Light/dark screenshots are `native-glass-splits-light.png` and `native-glass-splits-dark.png`; logs are `desktop-after.log` and `desktop-motion-test.log`.
- Release builds: desktop `macos/build/Build/Products/Release/Nanocodex.app` and mobile `apple/build-release/Build/Products/Release-iphonesimulator/Nanocodex.app` both built successfully. Logs: `desktop-release.log`, `ios-release.log`. The mobile artifact targets simulators; no connected phone is needed.
- Web: root `pnpm build --filter=nanocodex-web --filter=@nanocodex/connect-playground` completed all seven Turbo tasks, including dependency builds, TypeScript, Vite, and docs checks. Log: `web-build.log`. Standard bundler warnings about browser-externalized Node modules remain; this CSS change does not alter those imports.
- Browser evidence: the compiled client ran through Vite's static preview in Chromium. Desktop docs and mobile docs/Connect were inspected; light/dark mobile docs have no horizontal overflow. Reduced-transparency emulation produced opaque `rgb(255, 255, 255)` / `rgb(33, 33, 33)` backgrounds with `backdrop-filter: none`; reduced motion produced `scroll-behavior: auto`. Results: `web-visual-check.json` and `web-*.png`. Static preview has no account backend, so Connect's service-unavailable state is expected; this is visual evidence, not an authenticated account journey. Production Worker preview's HTTPS redirect loop was not counted as a successful runtime check.

The native performance test was run against the captured baseline and after the change. It reports first layout/input 287/38.5 ms before and 154.8/14.3 ms after; median tab switching 27.0 vs 29.3 ms; median long streaming snapshot 57.9 vs 59.6 ms. These are single runs with differing background load, not causal speedup measurements or release launch benchmarks. The strongest evidence for the pane change is final-size geometry and preserved editor identity. Both baseline and after logs include an existing AppKit re-entrant-layout warning; this continuation does not claim to resolve that separate warning. No GPU-memory or physical low-end-device performance claim is made.

The first phone run exposed a stale toolbar-order assertion already inconsistent with the baseline's centered create button. Its expected order was corrected. The new Dynamic Type test exposed a test-helper assumption that every distant lazy tab already exists; the helper now scrolls the actual row before requiring the target. A simulator test-runner IPC failure was handled by using a fresh dedicated simulator rather than interpreting it as application evidence.

Final iPhone simulator run: all four tests passed (115.2 seconds of test execution): browser-back/draft restoration/latest-activity ordering; accessibility text scaling and scrolling to an offscreen tab; independent drafts and queued steering; centered create control without duplicate title. Log: `ios-verified-tests.log`; screenshots and test metadata: `ios-verified-attachments/`.

The continuation patch was applied cleanly to the main `nanocodex` checkout after validation, preserving unrelated dirty work. No commit or deployment was made. Rebuilt artifacts remain in the isolated worktree; `continuation.patch` contains only the ten changed source/test/style files relative to the captured baseline.

Screenshot review then caught a tab wider than the phone at accessibility XXXL. The final implementation caps each stable tab width to its available geometry; the strengthened test verifies both width and fully onscreen toolbar bounds after navigation. That focused test passed (`ios-accessibility-final.log`), and the final mobile Release artifact was rebuilt successfully. `iphone-accessibility-tabs.png` shows the result; `iphone-toolbar-detail.png` is an inspection crop confirming the complete toolbar. The width follow-up was also applied to the main checkout.

Final iPad verification: the same accessibility scaling/navigation/toolbar-bounds test passed on a fresh iPad Pro 11-inch (M4), iOS 18.2 simulator (19.1 seconds). This exercises the pre–Liquid Glass material fallback. Log: `ipad18-ui-test.log`; screenshot: `ipad-accessibility-tabs.png`; attachments: `ipad-verified-attachments/`. iOS 26.5 iPad launch/install attempts stalled at the simulator service and are not counted as application results. The hosted agent GET endpoint still returned HTTP 503 on the final recheck; local completion does not depend on it.
