import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const indexCss = source("../src/index.css");
const terminalPresentationCss = source("../../js/terminal/styles.css");
const terminalCss = [
  source("../src/AgentTerminal.css"),
  terminalPresentationCss,
].join("\n");
const homeCss = source("../src/Home.css");
const sourceBrowserCss = source("../src/SourceBrowser.css");
const commitsCss = source("../src/Commits.css");
const docsCss = source("../src/Docs.css");
const evalsCss = source("../src/evals.css");
const application = source("../src/NanocodexApp.tsx");
const artifactRuntime = source("../src/artifactRuntime.tsx");
const experience = source("../src/AgentExperience.tsx");
const terminal = [
  source("../src/AgentTerminal.tsx"),
  source("../../js/terminal/src/AgentTerminalView.tsx"),
  source("../../js/terminal/src/TerminalTranscriptSurface.tsx"),
].join("\n");
const terminalComposer = source("../../js/terminal/src/TerminalComposer.tsx");
const artifactDock = source("../src/ArtifactDock.tsx");
const modelSession = source("../src/modelSession.tsx");
const docs = source("../src/Docs.tsx");
const modalBoundary = source("../src/modalBoundary.ts");
const modalFrameBoundary = source("../src/useModalFrameBoundary.ts");
const mobileInteraction = [
  source("../src/mobileInteraction.ts"),
  source("../../js/terminal/src/policy.ts"),
].join("\n");
const deploymentRollover = source("../src/useDeploymentRollover.ts");
const compactQuery = "(max-width: 740px), (pointer: coarse) and (orientation: landscape) and (max-width: 950px)";
const coarseQuery = "(pointer: coarse), (any-pointer: coarse)";

test("terminal and application controls share the compact phone policy", () => {
  assert.ok(indexCss.includes(`@media ${compactQuery} {`));
  assert.ok(terminalCss.includes(`@media ${compactQuery} {`));
  const compact = terminalCss.indexOf(`@media ${compactQuery}`);
  const auth = ruleBlock(terminalCss, ".agent-session-bar,", compact);
  const shell = ruleBlock(terminalCss, ".agent-terminal-shell {", compact);
  const previewShell = ruleBlock(
    terminalCss,
    ".nanocodex-demo.is-preview .agent-terminal-shell {",
    compact,
  );
  assert.match(auth, /min-height:\s*44px/);
  assert.match(shell, /100dvh/);
  assert.match(shell, /min-height:\s*220px/);
  assert.match(previewShell, /env\(safe-area-inset-bottom\)/);
});

test("the shared phone header stays in one compact row on every surface", () => {
  const phone = indexCss.indexOf("@media (max-width: 740px) {", indexCss.indexOf("@media (max-width: 1023px)"));
  const header = ruleBlock(indexCss, ".site-header {", phone);
  assert.match(header, /grid-template-columns:\s*auto minmax\(0, 1fr\) auto/);
  assert.match(header, /grid-template-rows:\s*48px/);
  assert.match(header, /height:\s*var\(--mobile-header-height\)/);
  assert.doesNotMatch(indexCss, /\.surface-code \.header-actions/);
  assert.doesNotMatch(indexCss, /\.surface-commits \.header-actions/);
});

test("phone headers expose readable product navigation in an owned modal", () => {
  const phone = indexCss.indexOf("@media (max-width: 740px) {", indexCss.indexOf("@media (max-width: 1023px)"));
  const narrow = indexCss.indexOf("@media (max-width: 420px)");
  assert.notEqual(narrow, -1);
  assert.match(ruleBlock(indexCss, ".header-center {", phone), /display:\s*none/);
  assert.match(ruleBlock(indexCss, ".mobile-navigation-trigger {", phone), /width:\s*44px/);
  assert.match(ruleBlock(indexCss, ".mobile-navigation-trigger {", phone), /height:\s*44px/);
  assert.match(ruleBlock(indexCss, ".mobile-navigation-grid {", phone), /repeat\(2, minmax\(0, 1fr\)\)/);
  assert.match(ruleBlock(indexCss, ".mobile-navigation-sections a {", phone), /min-height:\s*56px/);
  assert.match(ruleBlock(indexCss, ".wordmark {", narrow), /font-size:\s*10px/);
  assert.doesNotMatch(ruleBlock(indexCss, ".site-header {", narrow), /grid-template-rows:\s*48px 48px/);
  assert.doesNotMatch(indexCss, /--mobile-header-height:\s*calc\(96px/);
  assert.match(application, /className="mobile-product-navigation"/);
  assert.match(application, /role="dialog"[\s\S]*?aria-modal="true"[\s\S]*?Mobile product navigation/);
  assert.match(application, /useModalBoundary\(\{[\s\S]*?onDismiss: closeMobileNavigation/);
  assert.match(application, /onClick=\{toggleMobileNavigation\}/);
  assert.match(application, /className="mobile-navigation-backdrop"[\s\S]*?onClick=\{closeMobileNavigation\}/);
  assert.doesNotMatch(application, /onPointerDown=\{closeMobileNavigation\}/);
  assert.match(application, /<AccountMenu/);
  assert.match(application, /<span>\{item\.label\}<\/span><small>\{item\.description\}<\/small>/);
  assert.match(application, /className="mobile-navigation-group" aria-labelledby="mobile-demos-title"/);
  assert.match(application, /className="mobile-navigation-group" aria-labelledby="mobile-git-title"/);
  assert.match(application, /href=\{connectDemoUrl\}[\s\S]*?External demo/);
  assert.match(ruleBlock(indexCss, ".header-install-trigger {", narrow), /width:\s*44px/);
  assert.match(ruleBlock(indexCss, ".header-install-trigger span {", narrow), /display:\s*none/);
});

test("portrait commits still collapse to an in-viewport viewer column", () => {
  const mobile = lastRuleBlock(indexCss, ".commits-workspace {");
  assert.match(mobile, /grid-template-columns:\s*minmax\(0,\s*1fr\)/);
  assert.match(mobile, /grid-template-areas:\s*"header"\s*"viewer"/);
});

test("the Source drawer is modal, scroll-locked, and touch-sized", () => {
  const sourceBrowser = source("../src/CodeBrowser.tsx");
  assert.match(sourceBrowser, /role=\{modalOpen \? "dialog" : "complementary"\}/);
  assert.match(sourceBrowser, /aria-modal=\{modalOpen \? true : undefined\}/);
  assert.match(sourceBrowser, /useModalBoundary\(\{[\s\S]*?onDismiss: closeTree,[\s\S]*?returnFocusRef: treeOpenerRef/);
  assert.match(sourceBrowser, /fallbackFocusRef: workspaceRef/);
  assert.match(modalBoundary, /createOutsideInertOwner/);
  assert.match(modalBoundary, /rootStyle\.overflow = bodyStyle\.overflow = "hidden"/);
  assert.match(modalBoundary, /rootStyle\.overscrollBehavior = bodyStyle\.overscrollBehavior = "none"/);
  assert.match(modalBoundary, /event\.key === "Escape"/);
  assert.match(sourceBrowserCss, /\.source-browser \.source-tree-toolbar button,[\s\S]*?min-width:\s*44px/);
  assert.match(sourceBrowserCss, /\.source-browser \.code-file-tail-error button,[\s\S]*?min-height:\s*44px/);
});

test("compact Artifact, Source, and Docs overlays share complete modal ownership", () => {
  assert.match(artifactDock, /const modalOpen = compact && !collapsed/);
  assert.match(artifactDock, /role=\{modalOpen \? "dialog" : "complementary"\}/);
  assert.match(artifactDock, /aria-modal=\{modalOpen \? true : undefined\}/);
  assert.match(artifactDock, /className="artifact-dock-backdrop"/);
  assert.match(artifactDock, /useModalBoundary\(\{[\s\S]*?onDismiss: collapse,[\s\S]*?returnFocusRef: toggleRef/);
  assert.match(artifactDock, /useModalFrameBoundary\(\{[\s\S]*?onDismiss: collapse/);
  assert.match(docs, /role="dialog"[\s\S]*?aria-modal="true"/);
  assert.match(docs, /useModalBoundary\(\{[\s\S]*?onDismiss: closeBrowse,[\s\S]*?returnFocusRef: browseButtonRef/);
  assert.match(docs, /fallbackFocusRef: desktopFocusRef/);
  assert.match(modalBoundary, /new MutationObserver\(inertOwner\.refresh\)/);
  assert.match(modalBoundary, /document\.addEventListener\("focusin", onFocusIn, true\)/);
  assert.match(modalFrameBoundary, /frame\.contentWindow !== event\.source/);
  assert.match(modalBoundary, /iframe/);
  assert.match(modalBoundary, /contenteditable/);
  assert.match(modalBoundary, /summary:first-of-type/);
  assert.match(modalBoundary, /orderModalTabSequence/);
  assert.match(modalBoundary, /isRadioTabStop/);
  assert.match(modalBoundary, /contentVisibility !== "hidden"/);
  assert.match(artifactRuntime, /modalFrameBoundaryMessage\("Escape"\)/);
  assert.match(artifactRuntime, /modalFrameTabBoundaryKey\(\{/);
  assert.match(artifactRuntime, /!modalBoundaryActive/);
  assert.match(modalFrameBoundary, /setState\(true\)/);
  assert.match(modalFrameBoundary, /setState\(false\)/);
});

test("the homepage is one local, terminal-first agent on desktop and mobile", () => {
  assert.match(application, /<AgentExperience[\s\S]*?mode=\{[\s\S]*?"full"[\s\S]*?"preview"[\s\S]*?"hidden"[\s\S]*?theme=\{theme\}/);
  assert.equal(matches(application, /<AgentExperience\b/g), 1);
  assert.match(application, /landing=\{agentExperienceSurface === "home"\}/);
  assert.match(application, /hidden=\{surface !== "home" && surface !== "agent"\}/);
  assert.match(application, /inert=\{surface !== "home" && surface !== "agent" \? true : undefined\}/);
  assert.doesNotMatch(application, /home-intro|home-install|home-demo-head|live agent · local or durable/);
  assert.match(experience, /landing \? null : <ConversationHistoryRail/);
  assert.match(experience, /runtime="managed"/);
  assert.doesNotMatch(experience, /agent-runtime-switch|Local browser|Managed durable/);
  assert.match(experience, /High-performance Codex SDK\. Runs anywhere\./);
  assert.match(experience, /curl -fsSL https:\/\/nanocodex\.paradigm\.xyz \| bash/);
  assert.match(experience, /Terminal-Bench 2\.1 high · 82\.2% · 890\/890 runs/);
  assert.match(terminal, /import \{ Streamdown \} from "streamdown"/);
  assert.match(terminal, /className="agent-dom-transcript"/);
  assert.doesNotMatch(terminal, /Copy terminal transcript|agent-terminal-toolbar/);
  assert.match(terminal, /<Streamdown[\s\S]*?mode=\{entry\.streaming \? "streaming" : "static"\}/);
  assert.match(terminalCss, /--terminal-background:\s*var\(--surface\)/);
  assert.match(homeCss, /\.home-page \{[\s\S]*?height:\s*100%/);
  assert.match(homeCss, /overflow:\s*hidden/);
  assert.match(homeCss, /\.home-page \.agent-terminal-shell \{[\s\S]*?height:\s*100%/);
  assert.doesNotMatch(terminal, /<NanocodexTui|<WorkspacePanel/);
  assert.match(terminal, /accessory=\{\([\s\S]*?<ArtifactDock/);
  assert.match(terminal, /return mode === "full" \? \([\s\S]*?accessory\?\.\([\s\S]*?\) : terminal/);
});

test("the app shell owns deployment rollover and agent failures expose only manual retry", () => {
  assert.match(application, /useDeploymentRollover\(\)/);
  assert.match(deploymentRollover, /event\.persisted/);
  assert.match(deploymentRollover, /liveDeploymentSha === currentDeploymentSha/);
  assert.match(deploymentRollover, /window\.location\.reload\(\)/);
  assert.match(application, /beforeLocalTurn=\{deploymentRollover\.beforeLocalTurn\}/);
  assert.match(experience, /deploymentCurrent[\s\S]*?<AgentTerminal/);
  assert.match(terminal, /refetch\(\)/);
  assert.match(modelSession, /agentStatus === "error" && hasCredential[\s\S]*?>retry agent<\/button>/);
  assert.doesNotMatch(`${terminal}\n${modelSession}`, /automaticRetry|workerRecoveryAttempts/);
  assert.doesNotMatch(source("../src/AgentTerminal.tsx"), /setTimeout\(/);
  assert.doesNotMatch(terminal, /deployment_sha|pageshow/);
  assert.doesNotMatch(terminal, /createDemoAgent|setRetryGeneration|sessions\.current\.replace/);
});

test("touch terminals use one native IME-safe composer and one contextual action", () => {
  const touchComposer = terminalComposer;
  assert.match(mobileInteraction, /COARSE_POINTER_QUERY = "\(pointer: coarse\), \(any-pointer: coarse\)"/);
  assert.match(touchComposer, /window\.matchMedia\(COARSE_POINTER_QUERY\)/);
  assert.equal(matches(terminal, /<TerminalComposer\b/g), 1);
  assert.match(touchComposer, /<textarea[\s\S]*?aria-label="Message Nanocodex"/);
  assert.doesNotMatch(touchComposer, /placeholder="Message Nanocodex"/);
  assert.match(touchComposer, /terminalComposerAction\(running, draft\)/);
  assert.match(touchComposer, /aria-label="Stop response"/);
  assert.match(touchComposer, /<Square aria-hidden="true"/);
  assert.match(touchComposer, /aria-label="Send message"/);
  assert.match(touchComposer, /<ArrowUp aria-hidden="true"/);
  assert.equal(matches(touchComposer, /className="agent-touch-actions"/g), 1);
  assert.match(touchComposer, /className="agent-touch-actions">\s*\{controls\}[\s\S]*?aria-label="Send message"/);
  assert.match(terminal, /className="agent-voice-button"[\s\S]*?aria-label=\{engaged \? "Stop voice" : "Start voice"\}/);
  assert.equal(matches(touchComposer, /className="agent-touch-field"/g), 1);
  assert.doesNotMatch(touchComposer, />Steer<|>Queued</);
  assert.doesNotMatch(touchComposer, /scrollHeight/);
  assert.match(ruleBlock(terminalPresentationCss, ".agent-touch-composer textarea {", terminalPresentationCss.indexOf(`@media ${coarseQuery}`)), /field-sizing:\s*content/);
  assert.doesNotMatch(touchComposer, /agent-touch-rail|>│<\/span>/);
  assert.doesNotMatch(touchComposer, /\x1b\[200~|bracketed-paste/i);
  assert.match(terminal, /useAgentController\(agent/);
  assert.doesNotMatch(terminal, /inputMode: "composer"|setInputMode\(/);

  const touchCss = terminalPresentationCss.indexOf("@media (pointer: coarse), (any-pointer: coarse)");
  assert.notEqual(touchCss, -1);
  assert.match(ruleBlock(terminalPresentationCss, ".agent-touch-composer textarea {", touchCss), /font:\s*400 16px/);
  assert.match(ruleBlock(terminalPresentationCss, ".agent-touch-actions button {", touchCss), /min-height:\s*44px/);
  const composer = ruleBlock(terminalPresentationCss, ".agent-touch-composer {", touchCss);
  assert.match(composer, /position:\s*relative/);
  assert.match(composer, /min-height:\s*var\(--terminal-composer-min-height/);
  assert.match(terminalCss, /--terminal-safe-area-bottom:\s*env\(safe-area-inset-bottom\)/);
  assert.match(terminalCss, /--terminal-composer-lift:\s*max\([\s\S]*?var\(--terminal-keyboard-inset, 0px\) - var\(--terminal-safe-area-bottom\)/);
  assert.match(terminalCss, /--terminal-composer-min-height:\s*calc\(62px \+ var\(--terminal-safe-area-bottom\)\)/);
  assert.doesNotMatch(terminalCss, /data-agent-keyboard/);
  assert.match(composer, /env\(safe-area-inset-left\)/);
  assert.match(composer, /env\(safe-area-inset-right\)/);
  const field = ruleBlock(terminalPresentationCss, ".agent-touch-field {", touchCss);
  assert.match(field, /background:\s*var\(--terminal-background/);
  assert.match(field, /border-left:\s*1px solid var\(--terminal-muted/);
  assert.match(field, /border-radius:\s*0/);
  const action = ruleBlock(terminalPresentationCss, ".agent-touch-actions button {", touchCss);
  assert.match(action, /background:\s*transparent/);
  assert.match(action, /border-radius:\s*0/);
  assert.match(terminalPresentationCss, /\.agent-touch-composer\.is-running \.agent-touch-actions button \{[\s\S]*?color:\s*var\(--terminal-muted/);
});

test("the phone transcript owns the remaining workspace and native vertical gestures", () => {
  const compact = terminalCss.indexOf(
    `@media ${compactQuery}`,
    terminalCss.indexOf(".conversation-list"),
  );
  const compactCss = terminalCss.slice(compact);
  const workspace = ruleBlock(compactCss, ".conversation-workspace {");
  const main = ruleBlock(compactCss, ".conversation-main {");
  const transcript = ruleBlock(terminalPresentationCss, ".agent-dom-transcript {");

  assert.match(workspace, /grid-template-rows:\s*44px minmax\(0, 1fr\)/);
  assert.match(main, /grid-row:\s*2/);
  assert.match(main, /min-height:\s*0/);
  assert.match(transcript, /overflow:\s*auto/);
  assert.match(transcript, /overscroll-behavior:\s*contain/);
  assert.match(transcript, /-webkit-overflow-scrolling:\s*touch/);
  assert.match(terminalCss, /\.nanocodex-demo\.is-landing \.conversation-workspace \{[\s\S]*?grid-template-rows:\s*minmax\(0, 1fr\)/);
  assert.match(homeCss, /\.home-page \{[\s\S]*?overflow:\s*hidden/);
  assert.match(application, /const terminalSurfaceActive = surface === "home" \|\| surface === "agent"/);
  assert.match(application, /if \(!terminalSurfaceActive\) return;[\s\S]*?classList\.add\("agent-viewport-locked"\)[\s\S]*?lockDocumentScroll\(root, body\)/);
  assert.match(application, /className=\{`site-shell surface-\$\{surface\}`\} ref=\{shellRef\}/);
  assert.match(application, /\}, \[terminalSurfaceActive\]\)/);
  assert.match(indexCss, /html\.agent-viewport-locked,[\s\S]*?body\.agent-viewport-locked \{[\s\S]*?height:\s*100%;[\s\S]*?min-height:\s*0/);
  const fixedBody = ruleBlock(indexCss, "body.agent-viewport-locked {", indexCss.indexOf("body.agent-viewport-locked {") + 1);
  const fixedSurface = ruleBlock(indexCss, ".surface-home,", indexCss.lastIndexOf(".surface-home,"));
  assert.match(fixedBody, /position:\s*fixed/);
  assert.match(fixedBody, /inset:\s*0/);
  assert.match(fixedSurface, /position:\s*fixed/);
  assert.match(fixedSurface, /inset:\s*0/);
  assert.match(application, /const viewport = window\.visualViewport/);
  assert.doesNotMatch(application, /agentSurface\.style\.(?:width|height)\s*=/);
  assert.doesNotMatch(application, /agentSurface\.style\.transform\s*=/);
  assert.match(application, /viewport\?\.addEventListener\("resize", anchorViewport\)/);
  assert.match(application, /viewport\?\.addEventListener\("scroll", anchorViewport\)/);
  assert.match(application, /visualViewportKeyboardInset\(\{[\s\S]*?baselineHeight:\s*agentSurface\.clientHeight,[\s\S]*?viewportHeight:\s*viewport\.height,[\s\S]*?viewportOffsetTop:\s*viewport\.offsetTop/);
  assert.match(application, /style\.setProperty\("--terminal-keyboard-inset", `\$\{keyboardInset\}px`\)/);
  assert.match(terminalPresentationCss, /\.agent-touch-composer \{[\s\S]*?transform:\s*translate3d\([\s\S]*?var\(--terminal-composer-lift/);
  assert.match(terminalPresentationCss, /\.agent-transcript-keyboard-spacer \{[\s\S]*?height:\s*var\(--terminal-composer-lift/);
  assert.match(terminal, /className="agent-transcript-keyboard-spacer" aria-hidden="true"/);
  assert.match(terminal, /observer\.observe\(element\)/);
});

test("compact agent chrome prioritizes the conversation and transcript", () => {
  const compact = terminalCss.indexOf(`@media ${compactQuery}`);
  assert.match(modelSession, /agent-session-shell\$\{compactReady \? " is-compact-ready" : ""\}/);
  assert.match(ruleBlock(terminalCss, ".agent-session-shell.is-compact-ready {", compact), /display:\s*none/);
  assert.match(application, /className="mobile-product-navigation"/);
  assert.match(artifactDock, /artifacts\.length === 0 \? " is-empty"/);
  assert.match(ruleBlock(terminalCss, ".nanocodex-demo.is-full .artifact-dock.is-collapsed.is-empty {", compact), /display:\s*none/);
  const conversationRail = source("../src/ConversationHistoryRail.tsx");
  assert.match(conversationRail, /className="conversation-mobile-title"/);
  assert.match(conversationRail, /className="conversation-mobile-new"/);
  assert.match(conversationRail, /statusLabel\(agentStatus\)/);
  assert.doesNotMatch(conversationRail, /conversation\.id\.slice/);
  assert.match(ruleBlock(terminalCss, ".conversation-row {", terminalCss.indexOf(`@media ${compactQuery}`, compact + 1)), /min-height:\s*62px/);
});

test("the website presents the public headless controller without duplicating its lifecycle", () => {
  assert.match(terminal, /useAgentController\(agent, \{/);
  assert.match(terminal, /entries=\{controller\.entries\}/);
  assert.match(terminal, /controller\.running \|\| controller\.pendingTurns > 0/);
  assert.doesNotMatch(terminal, /createAgentTerminal|agent\.turn\.prompt/);
  assert.doesNotMatch(terminal, /new Worker|postMessage/);
});

test("the artifact runtime remains independently scrollable", () => {
  assert.ok(artifactRuntime.includes('document.documentElement.classList.add("artifact-runtime-page")'));
  assert.match(indexCss, /\.artifact-runtime-page body \{[\s\S]*?min-width:\s*0;[\s\S]*?overflow-y:\s*auto;[\s\S]*?\}/);
});

test("phone auth controls and other application targets meet mobile baselines", () => {
  assert.match(ruleBlock(terminalCss, ".agent-session-actions button,", terminalCss.indexOf(`@media ${compactQuery}`)), /min-height:\s*44px/);
  assert.match(ruleBlock(terminalCss, ".conversation-list-error button {"), /min-height:\s*44px/);

  for (const selector of [
    ".pierre-tree-heading button",
    ".mobile-tree-toggle",
    ".mobile-drawer-close",
    ".code-file-search",
    ".commit-view-button",
    ".commit-query button",
    ".commit-indicator-options button",
  ]) {
    const block = lastRuleBlock(indexCss, `${selector} {`);
    assert.match(block, /(?:width|min-width|min-height|height):\s*44px/, selector);
  }
  assert.ok(indexCss.includes(".commit-display-menu-item,\n  .commit-setting-row {\n    min-height: 44px;"));

  const phone = indexCss.indexOf("@media (max-width: 740px) {", indexCss.indexOf("@media (max-width: 1023px)"));
  const switcher = ruleBlock(indexCss, ".surface-switch {", phone);
  const surfaces = ruleBlock(indexCss, ".surface-switch > a,", indexCss.indexOf(".surface-switch > a,"));
  const brand = ruleBlock(indexCss, ".site-brand {", phone);
  const install = ruleBlock(indexCss, ".header-install-trigger {", phone);
  assert.match(switcher, /padding:\s*0/);
  assert.match(surfaces, /min-height:\s*44px/);
  assert.match(brand, /min-height:\s*44px/);
  assert.match(install, /min-height:\s*44px/);

  assert.match(ruleBlock(indexCss, ".search-field button {", phone), /width:\s*44px[\s\S]*?height:\s*44px/);
  assert.match(ruleBlock(indexCss, ".search-result {", phone), /min-height:\s*44px/);

  const coarseTerminal = terminalCss.indexOf(`@media ${coarseQuery}`);
  assert.match(ruleBlock(terminalCss, ".agent-oauth-code a,", coarseTerminal), /min-height:\s*var\(--coarse-target-size\)/);
  assert.match(ruleBlock(terminalCss, ".artifact-dock-header > select {", coarseTerminal), /height:\s*var\(--coarse-target-size\)/);
  assert.match(ruleBlock(terminalCss, ".artifact-preview-button {", coarseTerminal), /min-height:\s*var\(--coarse-target-size\)/);
  const coarseApplication = indexCss.indexOf(`@media ${coarseQuery}`);
  assert.match(
    ruleBlock(indexCss, ".requests-empty .button,", coarseApplication),
    /min-height:\s*var\(--coarse-target-size\)/,
  );
  assert.match(
    ruleBlock(indexCss, ".commit-stream-tail-error button,", coarseApplication),
    /min-height:\s*var\(--coarse-target-size\)/,
  );
});

test("expanded artifact docks stay inside every safe-area edge", () => {
  const fullscreen = ruleBlock(terminalCss, ".nanocodex-demo.is-full .artifact-dock.is-fullscreen {", 0);
  const compact = terminalCss.indexOf(`@media ${compactQuery}`);
  const mobile = ruleBlock(
    terminalCss,
    ".nanocodex-demo.is-full .artifact-dock:not(.is-collapsed) {",
    compact,
  );

  for (const inset of ["top", "right", "bottom", "left"]) {
    assert.match(fullscreen, new RegExp(`max\\(18px, env\\(safe-area-inset-${inset}\\)\\)`));
    assert.match(mobile, new RegExp(`max\\(10px, env\\(safe-area-inset-${inset}\\)\\)`));
  }
});

test("portrait coarse-pointer tablets retain 44px controls without changing layout", () => {
  for (const [css, selector] of [
    [indexCss, ".header-install-trigger,"],
    [terminalCss, ".agent-session-bar,"],
    [sourceBrowserCss, ".source-browser .source-tree-toolbar button {"],
    [commitsCss, ".commits-workspace .commit-scope-tabs button,"],
    [evalsCss, ".eval-back,"],
    [docsCss, ".docs-sidebar a,"],
  ] as const) {
    const coarse = css.indexOf(`@media ${coarseQuery}`);
    assert.notEqual(coarse, -1, selector);
    assert.match(ruleBlock(css, selector, coarse), /min-height:\s*44px/, selector);
  }
});

test("the terminal chrome delegates account and model connection controls to the account menu", () => {
  assert.match(experience, /useModelSession\(\{/);
  assert.doesNotMatch(experience, /<AgentSessionBar/);
  assert.doesNotMatch(modelSession, /Account agent/);
  assert.doesNotMatch(modelSession, /Connected to your ChatGPT subscription/);
  assert.doesNotMatch(modelSession, /The agent runs in your browser/);
  assert.match(modelSession, /aria-live="polite"/);
  assert.match(modelSession, /Connect ChatGPT or an OpenAI API key from the account menu/);
  assert.match(modelSession, /Sign in with a passkey from the account menu/);
  assert.doesNotMatch(`${terminal}\n${modelSession}`, /Tempo|MPP|payment details|onSelectTransport/);
});

function source(path: string): string {
  return readFileSync(new URL(path, import.meta.url), "utf8");
}

function lastRuleBlock(css: string, selector: string): string {
  const start = css.lastIndexOf(selector);
  assert.notEqual(start, -1, `missing ${selector}`);
  return ruleBlock(css, selector, start);
}

function ruleBlock(css: string, selector: string, from: number): string {
  const start = css.indexOf(selector, from);
  assert.notEqual(start, -1, `missing ${selector}`);
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  return css.slice(start, close + 1);
}

function matches(value: string, pattern: RegExp) {
  return [...value.matchAll(pattern)].length;
}
