// Generated from the runtime browser API manifest. Run `pnpm generate:api` after changing it.

export type BrowserCapabilityCollection = {
  get(id: string): Promise<unknown>;
  list(): Promise<Array<{ id: string; description: string }>>;
};

export interface BrowserHistoryOptions {
  from?: string | Date; // Lower bound for visit timestamps.
  limit?: number; // Maximum number of history entries to return.
  queries?: Array<string>; // Optional terms to filter browser history with.
  to?: string | Date; // Upper bound for visit timestamps.
}

export interface BrowserHistoryEntry {
  dateVisited: string; // ISO 8601 timestamp for the visit.
  title?: string; // Page title captured for the visit.
  url: string; // Visited URL.
}

export interface BrowserUserTabInfo {
  id: string; // Opaque identifier for this browser tab.
  lastOpened?: string; // ISO 8601 timestamp for the last time the tab was opened or focused.
  providerTabId?: string; // Provider-owned identity for correlating an explicit reference with this fresh listing.
  tabGroup?: string; // User-visible tab group name when the tab belongs to one.
  title?: string; // User-visible tab title.
  url?: string; // Current tab URL.
}

export interface TabsContentOptions {
  contentType: TabsContentType; // Content representation to extract from each page.
  timeoutMs?: number; // Maximum time to wait for each page load, in milliseconds.
  urls: Array<string>; // URLs to load in temporary background tabs.
}

export interface TabsContentResult {
  content: null | string; // Extracted page content or null if the page failed to load or extract.
  title: null | string; // The resolved page title when available.
  url: string; // The resolved page URL when available, otherwise the requested URL.
}

export interface TabInfo {
  id: string; // Metadata describing an open tab.
  providerTabId?: string; // Provider-owned identifier for matching an explicitly mentioned tab.
  title?: string;
  url?: string;
}

export type TabCapabilityCollection = {
  get(id: string): Promise<unknown>;
  list(): Promise<Array<{ id: string; description: string }>>;
};

export type Dialog = AlertDialog | BeforeUnloadDialog | ConfirmDialog | PromptDialog;

export type ScreenshotOptions = {
  clip?: ClipRect; // Crop to a specific rectangle instead of the full viewport.
  fullPage?: boolean; // Capture the full page instead of the viewport.
};

export type AXPoint = [unknown, unknown];

export type AXClickOptions = {
  clickCount?: number;
  mouseButton?: AXMouseButton;
};

export type AXStateOptions = {
  disableDiffing?: boolean;
};

export type AXDirection = "up" | "down" | "left" | "right" | "u" | "d" | "l" | "r";

export type AXSelectTextOptions = {
  prefix?: string;
  selectionType?: AXSelectionType;
  suffix?: string;
};

export type ClickOptions = {
  button?: number; // Mouse button (1-left, 2-middle/wheel, 3-right, 4-back, 5-forward).
  keypress?: Array<string>; // Modifier keys held during the click.
  x: number;
  y: number;
};

export type DoubleClickOptions = {
  keypress?: Array<string>; // Modifier keys held during the double click.
  x: number;
  y: number;
};

export type CuaDownloadMediaOptions = {
  timeoutMs?: number;
  x: number;
  y: number;
};

export type DragOptions = {
  keys?: Array<string>; // Optional modifier keys held during the drag.
  path: Array<{ x: number; y: number }>; // Drag path as a list of points.
};

export type KeypressOptions = {
  keys: Array<string>; // Key combination to press.
};

export type MoveOptions = {
  keys?: Array<string>; // Optional modifier keys held while moving.
  x: number;
  y: number;
};

export type ScrollOptions = {
  keypress?: Array<string>; // Modifier keys held during scroll.
  scrollX: number;
  scrollY: number;
  x: number;
  y: number;
};

export type TypeOptions = {
  text: string;
};

export type DomClickOptions = {
  node_id: string; // Node id from `get_visible_dom()`.
};

export type DomDownloadMediaOptions = {
  node_id: string; // Node id from `get_visible_dom()`.
  timeoutMs?: number;
};

export type DomKeypressOptions = {
  keys: Array<string>; // Key combination to press.
};

export type DomScrollOptions = {
  node_id?: string; // Optional node id to scroll within.
  x: number; // Horizontal scroll delta.
  y: number; // Vertical scroll delta.
};

export type DomTypeOptions = {
  text: string; // Text to type into the currently focused element.
};

export type ElementInfoOptions = {
  includeNonInteractable?: boolean; // When true, include non-interactable elements in addition to interactable targets.
  x: number;
  y: number;
};

export type ElementInfo = {
  ariaName?: string | null; // Accessible name if available.
  boundingBox?: ElementInfoRect | null; // Element bounds in screenshot coordinates.
  nodeId?: number | null; // Backend node id that can be passed to DOM-inspection APIs when available.
  preview: string; // Compact human-readable node preview.
  role?: string | null; // Computed ARIA role if available.
  selector: ElementInfoSelector; // Suggested selector data for this element.
  tagName: string; // Lowercased HTML tag name.
  testId?: string | null; // Configured test id attribute if present.
  visibleText?: string | null; // Rendered visible text, selected option text, or visible form value when available.
};

export type ElementScreenshotOptions = {
  includeNonInteractable?: boolean; // When true, highlight non-interactable elements in addition to interactable targets.
  x: number;
  y: number;
};

export type PlaywrightEvaluateFunction<TArg, TResult> = string | ((arg: TArg) => TResult | Promise<TResult>);

export type PlaywrightEvaluateOptions = {
  timeoutMs?: number; // Maximum time to spend setting up the read-only DOM scope and running the script.
};

export type LoadState = "load" | "domcontentloaded" | "networkidle";

export type TextMatcher = string | RegExp;

export type WaitForEventOptions = {
  timeoutMs?: number;
};

export type PageWaitForLoadStateOptions = {
  state?: LoadState;
  timeoutMs?: number;
};

export type PageWaitForURLOptions = {
  timeoutMs?: number;
  waitUntil?: WaitUntil;
};

export type LocatorCheckOptions = {
  force?: boolean;
  timeoutMs?: number;
};

export type LocatorClickOptions = {
  button?: MouseButton;
  force?: boolean;
  modifiers?: Array<KeyboardModifier>;
  timeoutMs?: number;
};

export type LocatorDownloadMediaOptions = {
  timeoutMs?: number;
};

export type LocatorEvaluateFunction<TArg, TResult> = string | ((element: Element, arg: TArg) => TResult | Promise<TResult>);

export type LocatorEvaluateAllFunction<TArg, TResult> = string | ((elements: Array<Element>, arg: TArg) => TResult | Promise<TResult>);

export type LocatorFilterOptions = {
  has?: PlaywrightLocator;
  hasNot?: PlaywrightLocator;
  hasNotText?: TextMatcher;
  hasText?: TextMatcher;
  visible?: boolean;
};

export type LocatorLocatorOptions = {
  has?: PlaywrightLocator;
  hasNot?: PlaywrightLocator;
  hasNotText?: TextMatcher;
  hasText?: TextMatcher;
};

export type LocatorPressSequentiallyOptions = {
  timeoutMs?: number;
};

export type SelectOptionInput = string | SelectOptionDescriptor;

export type LocatorWaitForOptions = {
  state: WaitForState;
  timeoutMs?: number;
};

export type FileChooserFiles = string | Array<string>;

export type TabClipboardItem = {
  entries: Array<TabClipboardEntry>;
  presentationStyle?: "unspecified" | "inline" | "attachment";
};

export interface TabDevLogsOptions {
  filter?: string; // Optional substring filter applied to the rendered log message.
  levels?: Array<"debug" | "info" | "log" | "warn" | "error" | "warning">; // Optional levels to include.
  limit?: number; // Maximum number of logs to return.
}

export interface TabDevLogEntry {
  level: "debug" | "info" | "log" | "warn" | "error"; // Console log level.
  message: string; // Rendered log message text.
  timestamp: string; // ISO 8601 timestamp for when the runtime captured the log.
  url?: string; // Source URL reported by the browser runtime, when available.
}

export type TabsContentType = "html" | "text" | "domSnapshot";

export type ClipRect = {
  height: number;
  width: number;
  x: number;
  y: number;
};

export type AXMouseButton = "left" | "right" | "middle" | "l" | "r" | "m";

export type AXSelectionType = "text" | "cursor_before" | "cursor_after";

export type ElementInfoRect = {
  height: number;
  width: number;
  x: number;
  y: number;
};

export type ElementInfoSelector = {
  candidates: Array<string>; // Ranked selector candidates for the element.
  frameSelectors?: Array<string>; // Frame selectors to enter before using the element selector.
  primary?: string | null; // The preferred selector for the element when available.
};

export type WaitUntil = LoadState | "commit";

export type MouseButton = "left" | "right" | "middle";

export type KeyboardModifier = "Alt" | "Control" | "ControlOrMeta" | "Meta" | "Shift";

export type SelectOptionDescriptor = {
  index?: number;
  label?: string;
  value?: string;
};

export type WaitForState = "attached" | "detached" | "visible" | "hidden";

export type TabClipboardEntry = {
  base64?: string;
  mimeType: string;
  text?: string;
};

export interface Agent {
  browsers: Browsers; // API for finding and selecting browsers.
  documentation: Documentation; // API for reading packaged browser-use documentation by name.
}

export interface Browsers {
  get(id: string): Promise<Browser>; // Get a browser by id or client type.
  getDefault(): Promise<Browser>; // Get the default browser from those currently available.
  getForUrl(url: string): Promise<Browser>; // Get the browser best suited to interact with the provided URL.
  list(): Promise<Array<{ family?: string; id: string; metadata?: { codexSessionId?: string; extensionInstanceId?: string }; name: string; profileName?: string; type: "iab" | "extension" | "cdp" }>>; // List available browsers.
}

export interface Browser {
  browserId: string; // Browser id selected by `agent.browsers.get()`.
  capabilities: BrowserCapabilityCollection; // Browser-scoped optional capabilities advertised by the connected backend; discover IDs with `await browser.capabilities.list()`, then call `await (await browser.capabilities.get(id)).documentation()` for method details.
  tabs: Tabs; // API for interacting with browser tabs.
  user: BrowserUser; // Context for user-owned browser tabs.
  documentation(): Promise<string>; // Read browser guidance and the core API reference.
  history(options: BrowserHistoryOptions): Promise<Array<BrowserHistoryEntry>>; // List recent browsing history ordered by `dateVisited` descending.
  nameSession(name: string): Promise<void>; // Name the current browser automation session.
}

export interface BrowserUser {
  claimTab(tab: string | BrowserUserTabInfo): Promise<Tab>; // Claim a user tab returned by `openTabs()` and return it as a controllable agent tab.
  openTabs(): Promise<Array<BrowserUserTabInfo>>; // List open top-level tabs across the user's browser windows ordered by `lastOpened` descending.
}

export interface Tabs {
  content(options: TabsContentOptions): Promise<Array<TabsContentResult>>; // Load one or more URLs in temporary background tabs and extract their content without changing the selected tab.
  get(id: string): Promise<Tab>; // Get a tab by id.
  list(): Promise<Array<TabInfo>>; // List open tabs in the browser.
  new(): Promise<Tab>; // Create and return a new tab in the browser.
  selected(): Promise<undefined | Tab>; // Return the currently selected tab, if any.
}

export interface Tab {
  ax: AXAPI; // API for interacting with accessibility state and accessibility elements.
  capabilities: TabCapabilityCollection; // Tab-scoped optional capabilities advertised by the connected backend; discover IDs with `await tab.capabilities.list()`, then call `await (await tab.capabilities.get(id)).documentation()` for method details.
  clipboard: TabClipboardAPI; // API for interacting with the browser session's clipboard.
  content: ContentAPI; // API for exporting tab content.
  cua: CUAAPI; // API for interacting with the tab via the cua api
  dev: TabDevAPI; // API for developer-oriented tab inspection.
  dom_cua: DomCUAAPI; // API for interacting with the tab via the dom based cua api
  id: string; // A tab's unique identifier
  playwright: PlaywrightAPI; // API for interacting with the tab via the playwright api
  back(): Promise<void>; // Navigate this tab back in history.
  close(): Promise<void>; // Close this tab.
  forward(): Promise<void>; // Navigate this tab forward in history.
  getJsDialog(): Promise<undefined | Dialog>; // Get the active JavaScript dialog for this tab, if one is currently open.
  goto(url: string): Promise<void>; // Open a URL in this tab.
  markDeliverable(): Promise<void>; // Keep this tab as a deliverable after the turn completes.
  markHandoff(): Promise<void>; // Keep this tab available for a later turn after the current turn completes.
  reload(): Promise<void>; // Reload this tab.
  requestManualHandoff(): Promise<void>; // Request manual user control of this Cloud Browser tab.
  screenshot(options: ScreenshotOptions): Promise<Uint8Array>; // Capture a screenshot of this tab.
  title(): Promise<undefined | string>; // Get the current title for this tab.
  url(): Promise<undefined | string>; // Get the current URL for this tab.
}

export interface AXAPI {
  click(target: number | AXPoint, options?: AXClickOptions): Promise<void>; // Click an accessibility element or viewport coordinate.
  drag(from: AXPoint, to: AXPoint): Promise<void>; // Drag between two viewport coordinates.
  get(mode?: "state", options?: AXStateOptions): Promise<string>; // Return accessibility state without displaying it; prefer write() for model-visible observation.
  get(mode: "screenshot"): Promise<Uint8Array>; // Return screenshot bytes without displaying an image or advancing accessibility state.
  get(mode: "both", options?: AXStateOptions): Promise<{ screenshot?: Uint8Array; state: string }>; // Return accessibility state and screenshot bytes when available.
  performSecondaryAction(elementIndex: number, action: string): Promise<void>; // Invoke an additional action exposed by an accessibility element.
  pressKey(key: string): Promise<void>; // Press a key or key combination in the current tab.
  scroll(target: number | AXPoint, direction: AXDirection, pages?: number): Promise<void>; // Scroll an accessibility element or a viewport coordinate.
  selectText(elementIndex: number, text: string, options?: AXSelectTextOptions): Promise<void>; // Select text or position the cursor within an editable element.
  setValue(elementIndex: number, value: string): Promise<void>; // Set the value of an accessibility element.
  typeText(text: string): Promise<void>; // Type text into the currently focused element.
  write(mode?: "state", options?: AXStateOptions): Promise<void>; // Prefer this method to display current accessibility state to the model.
  write(mode: "screenshot"): Promise<void>; // Display a screenshot without advancing accessibility state.
  write(mode: "both", options?: AXStateOptions): Promise<void>; // Display accessibility state followed by its corresponding screenshot.
}

export interface ContentAPI {
  export(): Promise<string>; // Export the tab's content to a file on disk using the default asset-loader path.
  exportGsuite(type: "pdf" | "md" | "xlsx" | "csv" | "docx" | "pptx"): Promise<string>; // Export a Google Workspace tab using an explicit GSuite export type.
  exportYouTubeTranscript(): Promise<string>; // Export an HTTPS youtube.com or www.youtube.com /watch transcript to a UTF-8 .txt file.
}

export interface CUAAPI {
  click(options: ClickOptions): Promise<void>; // Click at a coordinate in the current viewport.
  double_click(options: DoubleClickOptions): Promise<void>; // Double click at a coordinate in the current viewport.
  downloadMedia(options: CuaDownloadMediaOptions): Promise<void>; // Trigger a media download at a viewport coordinate.
  drag(options: DragOptions): Promise<void>; // Drag from a point to a point by the provided path.
  keypress(options: KeypressOptions): Promise<void>; // Press control characters at the current focused element (focus it first via click/dblclick).
  move(options: MoveOptions): Promise<void>; // Move the mouse to a point by the provided x and y coordinates.
  scroll(options: ScrollOptions): Promise<void>; // Scroll by a delta from a specific viewport coordinate.
  type(options: TypeOptions): Promise<void>; // Type text at the current focus.
}

export interface DomCUAAPI {
  click(options: DomClickOptions): Promise<void>; // Click a DOM node by its id from the visible DOM snapshot.
  double_click(options: DomClickOptions): Promise<void>; // Double-click a DOM node by its id.
  downloadMedia(options: DomDownloadMediaOptions): Promise<void>; // Trigger a media download for a DOM node.
  get_visible_dom(): Promise<unknown>; // Return a filtered DOM with node ids for interactable elements.
  keypress(options: DomKeypressOptions): Promise<void>; // Press control characters at the currently focused element (focus it first via click/dblclick).
  scroll(options: DomScrollOptions): Promise<void>; // Scroll either the page or a specific node (if node_id provided) by deltas.
  type(options: DomTypeOptions): Promise<void>; // Type text into the currently focused element (focus via click first).
}

export interface PlaywrightAPI {
  domSnapshot(): Promise<string>; // Return a snapshot of the current DOM as a string, including expanded iframe body content when available.
  elementInfo(options: ElementInfoOptions): Promise<Array<ElementInfo>>; // Return locator-oriented metadata for elements at a screenshot coordinate.
  elementScreenshot(options: ElementScreenshotOptions): Promise<Uint8Array>; // Capture a screenshot of the current viewport annotated with matching element bounds and the probed point.
  evaluate<TResult, TArg>(pageFunction: PlaywrightEvaluateFunction<TArg, TResult>, arg?: TArg, options?: PlaywrightEvaluateOptions): Promise<TResult>; // Evaluate JavaScript in a read-only page scope.
  expectNavigation<T>(action: () => Promise<T>, options: { timeoutMs?: number; url?: string; waitUntil?: LoadState }): Promise<T>; // Expect a navigation triggered by an action.
  frameLocator(frameSelector: string): PlaywrightFrameLocator; // Create a frame-scoped locator builder.
  getByLabel(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by label text within the page.
  getByPlaceholder(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by placeholder text within the page.
  getByRole(role: string, options: { exact?: boolean; name?: TextMatcher }): PlaywrightLocator; // Find elements by ARIA role within the page.
  getByTestId(testId: string): PlaywrightLocator; // Find elements by test id within the page.
  getByText(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by text within the page.
  locator(selector: string): PlaywrightLocator; // Create a locator scoped to this tab.
  waitForEvent(event: "download", options?: WaitForEventOptions): Promise<PlaywrightDownload>; // Wait for the next event on the page.
  waitForEvent(event: "filechooser", options?: WaitForEventOptions): Promise<PlaywrightFileChooser>;
  waitForLoadState(options: PageWaitForLoadStateOptions): Promise<void>; // Wait for the page to reach a specific load state.
  waitForTimeout(timeoutMs: number): Promise<void>; // Wait for a fixed duration.
  waitForURL(url: string, options: PageWaitForURLOptions): Promise<void>; // Wait for the page URL to match the provided value.
}

export interface PlaywrightFrameLocator {
  frameLocator(frameSelector: string): PlaywrightFrameLocator; // Create a locator scoped to a nested frame.
  getByLabel(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by label within this frame.
  getByPlaceholder(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by placeholder within this frame.
  getByRole(role: string, options: { exact?: boolean; name?: TextMatcher }): PlaywrightLocator; // Find elements by ARIA role within this frame.
  getByTestId(testId: string): PlaywrightLocator; // Find elements by test id within this frame.
  getByText(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by text within this frame.
  locator(selector: string): PlaywrightLocator; // Create a locator scoped to this frame.
}

export interface PlaywrightLocator {
  all(): Promise<Array<PlaywrightLocator>>; // Resolve to a list of locators for each matched element.
  allTextContents(options: { timeoutMs?: number }): Promise<Array<string>>; // Return `textContent` for *all* elements matched by this locator.
  and(locator: PlaywrightLocator): PlaywrightLocator; // Return a locator matching elements that satisfy both this locator and `locator`.
  check(options: LocatorCheckOptions): Promise<void>; // Check a checkbox or switch-like control.
  click(options: LocatorClickOptions): Promise<void>; // Click the element matched by this locator.
  count(): Promise<number>; // Number of elements matching this locator.
  dblclick(options: LocatorClickOptions): Promise<void>; // Double-click the element matched by this locator.
  downloadMedia(options: LocatorDownloadMediaOptions): Promise<void>; // Trigger a download for the media or file link in the first matched element.
  evaluate<TResult, TArg>(pageFunction: LocatorEvaluateFunction<TArg, TResult>, arg?: TArg, options?: PlaywrightEvaluateOptions): Promise<TResult>; // Evaluate JavaScript in a read-only scope; the locator must resolve unambiguously to one element.
  evaluateAll<TResult, TArg>(pageFunction: LocatorEvaluateAllFunction<TArg, TResult>, arg?: TArg, options?: PlaywrightEvaluateOptions): Promise<TResult>; // Evaluate read-only JavaScript against all elements matched by this locator.
  fill(value: string, options: { timeoutMs?: number }): Promise<void>; // Replace the element's value with the provided text.
  filter(options: LocatorFilterOptions): PlaywrightLocator; // Narrow this locator by additional constraints.
  first(): PlaywrightLocator; // Return a locator pointing at the first matched element.
  getAttribute(name: string, options: { timeoutMs?: number }): Promise<null | string>; // Return an attribute value from the first matched element.
  getByLabel(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by label text, scoped to this locator.
  getByPlaceholder(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by placeholder text, scoped to this locator.
  getByRole(role: string, options: { exact?: boolean; name?: TextMatcher }): PlaywrightLocator; // Find elements by ARIA role, scoped to this locator.
  getByTestId(testId: string): PlaywrightLocator; // Find elements by test id, scoped to this locator.
  getByText(text: TextMatcher, options: { exact?: boolean }): PlaywrightLocator; // Find elements by text content, scoped to this locator.
  innerText(options: { timeoutMs?: number }): Promise<string>; // Return the rendered (visible) text of the first matched element.
  isEnabled(): Promise<boolean>; // Whether the first matched element is currently enabled.
  isVisible(): Promise<boolean>; // Whether the first matched element is currently visible.
  last(): PlaywrightLocator; // Return a locator pointing at the last matched element.
  locator(selector: string, options: LocatorLocatorOptions): PlaywrightLocator; // Create a descendant locator scoped to this locator.
  nth(index: number): PlaywrightLocator; // Return a locator pointing at the Nth matched element.
  or(locator: PlaywrightLocator): PlaywrightLocator; // Return a locator matching elements that satisfy either this locator or `locator`.
  press(value: string, options: { timeoutMs?: number }): Promise<void>; // Press a keyboard key while this locator is focused.
  pressSequentially(value: string, options: LocatorPressSequentiallyOptions): Promise<void>; // Focus the element and press each character in the text sequentially without clearing its existing value.
  selectOption(value: SelectOptionInput | Array<SelectOptionInput>, options: { timeoutMs?: number }): Promise<void>; // Select one or more options on a native `<select>` element.
  setChecked(checked: boolean, options: LocatorCheckOptions): Promise<void>; // Set a checkbox or switch-like control to a checked/unchecked state.
  textContent(options: { timeoutMs?: number }): Promise<null | string>; // Return the raw textContent of the first matched element (or null if missing).
  type(value: string, options: { timeoutMs?: number }): Promise<void>; // Type text into the element without clearing existing content.
  uncheck(options: LocatorCheckOptions): Promise<void>; // Uncheck a checkbox or switch-like control.
  waitFor(options: LocatorWaitForOptions): Promise<void>; // Wait for the element to reach a specific state.
}

export interface PlaywrightDownload {
  path(options: { timeoutMs?: number }): Promise<null | string>; // Return the local path to the downloaded file, if available.
}

export interface PlaywrightFileChooser {
  isMultiple(): boolean; // Whether the input allows selecting multiple files.
  setFiles(files: FileChooserFiles, options: { timeoutMs?: number }): Promise<void>; // Set the files for this chooser.
}

export interface TabClipboardAPI {
  read(): Promise<Array<TabClipboardItem>>; // Read clipboard items, including text and binary payloads.
  readText(): Promise<string>; // Read plain text from the browser clipboard.
  write(items: Array<TabClipboardItem>): Promise<void>; // Write clipboard items.
  writeText(text: string): Promise<void>; // Write plain text to the browser clipboard.
}

export interface TabDevAPI {
  logs(options: TabDevLogsOptions): Promise<Array<TabDevLogEntry>>; // Read console log messages captured for this tab.
}

export interface AlertDialog {
  type: "alert";
  dismiss(): Promise<void>;
}

export interface BeforeUnloadDialog {
  type: "beforeunload";
  dismiss(): Promise<void>;
}

export interface ConfirmDialog {
  type: "confirm";
  accept(): Promise<void>;
  dismiss(): Promise<void>;
}

export interface Documentation {
  get(name: string): Promise<string>; // Read packaged documentation by its extensionless relative path.
}

export interface PromptDialog {
  type: "prompt";
  accept(text: string): Promise<void>;
  dismiss(): Promise<void>;
}
