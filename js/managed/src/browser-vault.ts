import { validatePrivateVaultSave, validatePrivateVaultDetails } from "./browser-vault-save";
import type { BrowserBinding } from "agents/browser";
import type { ToolContext } from "nanocodex";

export type BrowserVaultRequest = Readonly<{
  vault_id: string;
  expected_origin: string;
  target_id: string;
  username_selector?: string;
  password_selector?: string;
  submit: boolean;
}>;
export type BrowserVaultLogin = Readonly<{ username: string; password: string }>;
/** Host only: authorize the current tool context before calling the private vault RPC. */
export type BrowserVaultResolver = (
  request: BrowserVaultRequest, context: ToolContext,
) => Promise<BrowserVaultLogin>;
export type BrowserVaultQuarantine = Readonly<{
  sessionId: string; targetId: string; loaderId: string; origin: string; vaultId: string; mode?: "one_time";
}>;

export function parseBrowserVaultRequest(value: unknown): BrowserVaultRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid vault login request");
  const input = value as Record<string, unknown>;
  const keys = ["vault_id", "expected_origin", "target_id", "username_selector", "password_selector", "submit"];
  const selectors = [input.username_selector, input.password_selector].filter(s => s !== undefined);
  if (Object.keys(input).some(key => !keys.includes(key))
    || typeof input.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(input.vault_id)
    || typeof input.target_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(input.target_id)
    || !isBrowserVaultOrigin(input.expected_origin) || typeof input.submit !== "boolean"
    || !selectors.length || selectors.some(s => typeof s !== "string" || !s.trim() || s.length > 512)
    || (selectors.length === 2 && input.username_selector === input.password_selector)) throw new Error("Invalid vault login request");
  return input as BrowserVaultRequest;
}

export function isBrowserVaultOrigin(value: unknown): value is string {
  if (typeof value !== "string") return false;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.origin === value && !url.username && !url.password;
  } catch { return false; }
}

// Fixed classification only; provider messages never cross the private transport.
export class PrivateBrowserNoActiveTouch extends Error {
  constructor() { super("Private browser has no active touch"); }
}

/** No SDK/debug ring or model dispatcher ever receives privileged CDP traffic. */
export class PrivateBrowserCdp {
  #id = 0;
  #closed = false;
  #pending = new Map<number, { cancellingTouch: boolean; resolve(value: any): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }>();
  #attachment: { targetId: string; sessionId: string } | undefined;
  readonly socket: WebSocket;
  get closed() { return this.#closed; }
  constructor(socket: WebSocket) {
    this.socket = socket;
    socket.accept();
    socket.addEventListener("message", event => {
      try {
        if (typeof event.data !== "string") return;
        const message = JSON.parse(event.data);
        if (message.method === "Target.detachedFromTarget" && message.params?.sessionId === this.#attachment?.sessionId) this.#attachment = undefined;
        const pending = this.#pending.get(message.id);
        if (!pending) return;
        this.#pending.delete(message.id);
        clearTimeout(pending.timer);
        // Never copy provider error text or exception details to an error.
        if (message.error) pending.reject(pending.cancellingTouch
          && message.error.code === -32602
          && message.error.message === "Must send a TouchStart first to start a new touch."
          ? new PrivateBrowserNoActiveTouch() : new Error("Private browser operation failed"));
        else pending.resolve(message.result);
      } catch { this.close(); }
    });
    socket.addEventListener("close", () => this.#reject());
    socket.addEventListener("error", () => this.#reject());
  }
  static async connect(browser: BrowserBinding, sessionId: string, signal?: AbortSignal): Promise<PrivateBrowserCdp> {
    // Limit cancellation to the upgrade. Retained sockets must not inherit a
    // completed tool call's signal or the handshake deadline: disconnecting
    // discards the isolated world's private snapshot references.
    signal?.throwIfAborted();
    const handshake = new AbortController();
    const abort = () => handshake.abort();
    signal?.addEventListener("abort", abort, { once: true });
    const timer = setTimeout(abort, 10_000);
    try {
      const response = await browser.fetch(`https://localhost/v1/devtools/browser/${encodeURIComponent(sessionId)}`, {
        headers: { Upgrade: "websocket" }, signal: handshake.signal,
      });
      if (!response.webSocket) throw new Error("Private browser unavailable");
      const cdp = new PrivateBrowserCdp(response.webSocket);
      if (handshake.signal.aborted || signal?.aborted) {
        cdp.close();
        throw new Error("Private browser connection cancelled");
      }
      return cdp;
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
    }
  }
  send(method: string, params: unknown = {}, sessionId?: string): Promise<any> {
    if (this.#closed) return Promise.reject(new Error("Private browser disconnected"));
    const id = ++this.#id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error("Private browser operation timed out"));
        this.close();
      }, 10_000);
      this.#pending.set(id, { resolve, reject, timer,
        cancellingTouch: method === "Input.dispatchTouchEvent" && (params as { type?: unknown } | null)?.type === "touchCancel" });
      try { this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })); }
      catch { clearTimeout(timer); this.#pending.delete(id); reject(new Error("Private browser operation failed")); }
    });
  }
  async attachTarget(targetId: string): Promise<{ sessionId: string }> {
    if (this.#closed) throw new Error("Private browser disconnected");
    if (this.#attachment?.targetId === targetId) return { sessionId: this.#attachment.sessionId };
    const previous = this.#attachment;
    this.#attachment = undefined;
    if (previous) await this.send("Target.detachFromTarget", { sessionId: previous.sessionId });
    const attached = await this.send("Target.attachToTarget", { targetId, flatten: true });
    if (typeof attached?.sessionId !== "string") throw new Error("Private browser attachment failed");
    this.#attachment = { targetId, sessionId: attached.sessionId };
    return { sessionId: attached.sessionId };
  }
  #reject() {
    this.#closed = true;
    this.#attachment = undefined;
    for (const entry of this.#pending.values()) { clearTimeout(entry.timer); entry.reject(new Error("Private browser disconnected")); }
    this.#pending.clear();
  }
  close() { this.#reject(); try { this.socket.close(1000, "Finished"); } catch { /* No provider errors escape. */ } }
}

/** Shared isolated-world submission policy. Page listeners are expected on an
 * authorized login origin. Destination guards are best effort: trusted page JS
 * already receives credentials and can make its own requests or stop propagation. */
const VAULT_FORM_SUBMISSION = `
  // CSP applies to native submit(), including calls that bypass submit events.
  // Multiple policies intersect; never alter a site's form method or handlers.
  const protectNativeForms = (blockAll = false) => {
    if (!document.head) return false;
    const policy = document.createElement('meta');
    policy.httpEquiv = 'Content-Security-Policy';
    policy.content = blockAll ? "form-action 'none'" : "form-action 'self'";
    document.head.appendChild(policy);
    return policy.isConnected;
  };
  const safeLoginForm = form => {
    if (!(form instanceof HTMLFormElement) || !form.isConnected || form.getRootNode() !== document
      || location.origin !== origin || (form.method.toLowerCase() !== 'post' && form.hasAttribute('method')) || (form.target && form.target !== '_self')) return false;
    const action = new URL(form.action, location.href);
    return action.origin === origin && !action.username && !action.password;
  };
  const safeSubmitter = el => (el instanceof HTMLButtonElement || el instanceof HTMLInputElement)
    && el.type === 'submit' && !el.name && !el.disabled && !el.matches(':disabled') && el.getAttribute('aria-disabled') !== 'true'
    && !['formaction','formmethod','formtarget','formenctype','formnovalidate'].some(a => el.hasAttribute(a));
  const submitLoginForm = (form, submitter) => {
    if (!safeLoginForm(form) || (submitter && (submitter.form !== form || !safeSubmitter(submitter)))) return false;
    let safe = true;
    const guard = event => {
      if (event.target !== form) return;
      if (!safeLoginForm(form) || (event.submitter && (event.submitter.form !== form || !safeSubmitter(event.submitter)))) {
        safe = false; event.preventDefault();
      }
    };
    window.addEventListener('submit', guard);
    try { HTMLFormElement.prototype.requestSubmit.call(form, submitter || undefined); }
    finally { window.removeEventListener('submit', guard); }
    return safe && safeLoginForm(form);
  };
`;

type PrivateBrowserChannel = Pick<PrivateBrowserCdp, "send"> & Partial<Pick<PrivateBrowserCdp, "attachTarget">>;
const attachPrivateTarget = (cdp: PrivateBrowserChannel, targetId: string) => cdp.attachTarget
  ? cdp.attachTarget(targetId) : cdp.send("Target.attachToTarget", { targetId, flatten: true });

/** Host-only, bounded continuation transport. The runtime's exclusive gate serializes
 * uses. Recreation/expiry discards transport, never the browser quarantine. */
export class PrivateBrowserContinuationSession {
  #current: { sessionId: string; identity: string; cdp: PrivateBrowserCdp } | undefined;
  #idle: ReturnType<typeof setTimeout> | undefined;
  readonly browser: BrowserBinding;
  readonly idleMs: number;
  constructor(browser: BrowserBinding, idleMs = 5 * 60_000) { this.browser = browser; this.idleMs = idleMs; }
  close() {
    clearTimeout(this.#idle);
    this.#idle = undefined;
    this.#current?.cdp.close();
    this.#current = undefined;
  }
  async run<T>(sessionId: string, identity: BrowserVaultIdentity, signal: AbortSignal,
    operation: (cdp: PrivateBrowserCdp) => Promise<T>): Promise<T> {
    signal.throwIfAborted();
    const key = JSON.stringify([identity.vault_id, identity.target_id, identity.expected_origin]);
    clearTimeout(this.#idle);
    if (this.#current && (this.#current.sessionId !== sessionId || this.#current.identity !== key || this.#current.cdp.closed)) this.close();
    if (!this.#current) {
      const cdp = await PrivateBrowserCdp.connect(this.browser, sessionId, signal);
      if (signal.aborted) { cdp.close(); signal.throwIfAborted(); }
      this.#current = { sessionId, identity: key, cdp };
    }
    const current = this.#current;
    const abort = () => this.close();
    signal.addEventListener("abort", abort, { once: true });
    try { signal.throwIfAborted(); return await operation(current.cdp); }
    catch (error) { this.close(); throw error; }
    finally {
      signal.removeEventListener("abort", abort);
      if (this.#current === current) this.#idle = setTimeout(() => this.close(), this.idleMs);
    }
  }
}

/** A fixed function, executed in a fresh isolated world. Selectors are data, never code.
 * Restrict to visible, same-origin login inputs in the top frame. Methodless forms
 * and formless SPAs retain their semantics with native submissions blocked by CSP. Atomic checks
 * and native setters are followed by input/change events and destination rechecks.
 */
export const BROWSER_VAULT_FILL_FUNCTION = `function(origin, usernameSelector, passwordSelector, username, password, submit, dryRun = false) {
  if (window !== window.top || location.origin !== origin || location.protocol !== "https:") return false;
  ${VAULT_FORM_SUBMISSION}
  const one = selector => { const nodes = document.querySelectorAll(selector); return nodes.length === 1 ? nodes[0] : null; };
  const user = usernameSelector === null ? null : one(usernameSelector);
  const pass = passwordSelector === null ? null : one(passwordSelector);
  const visible = input => {
    if (!(input instanceof HTMLInputElement) || !input.isConnected || input.disabled || input.matches(':disabled') || input.readOnly
      || input.getRootNode() !== document || input.closest('[inert]')) return false;
    const style = getComputedStyle(input), rect = input.getBoundingClientRect();
    if (style.visibility !== 'visible' || style.display === 'none' || Number(style.opacity) === 0
      || rect.width <= 0 || rect.height <= 0 || rect.left < 0 || rect.top < 0
      || rect.right > innerWidth || rect.bottom > innerHeight) return false;
    if (!input.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) return false;
    return document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2) === input;
  };
  if ((!user && !pass) || (usernameSelector !== null && (!visible(user) || !['text', 'email'].includes(user.type)))
    || (passwordSelector !== null && (!visible(pass) || pass.type !== 'password'))
    || (user && pass && (user === pass || user.form !== pass.form))) return false;
  const form = (user || pass).form;
  if (form && !safeLoginForm(form)) return false;
  const challenge = () => [...document.querySelectorAll('iframe,[id],[class]')].slice(0,5000).some(el =>
    el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) && /captcha|turnstile|challenge-platform/i.test([el.id, typeof el.className === 'string' ? el.className : '', el instanceof HTMLIFrameElement ? el.src : ''].join(' ')));
  const method = form && form.getAttribute('method');
  const formAttributes = form && JSON.stringify([...form.attributes].map(a=>[a.name,a.value]));
  const valid = () => location.origin === origin && location.protocol === 'https:' && !challenge()
    && (!form || safeLoginForm(form) && form.getAttribute('method') === method && JSON.stringify([...form.attributes].map(a=>[a.name,a.value])) === formAttributes)
    && (!user || (one(usernameSelector) === user && user.form === form && visible(user) && ['text','email'].includes(user.type)))
    && (!pass || (one(passwordSelector) === pass && pass.form === form && visible(pass) && pass.type === 'password'));
  if (dryRun) return valid();
  if (!valid() || !protectNativeForms(!form || form.method.toLowerCase() !== 'post')) return false;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  for (const [input, value] of [[user, username], [pass, password]]) {
    if (!input) continue;
    if (!valid()) return false;
    setter.call(input, value);
    input.dispatchEvent(new Event('input', {bubbles:true}));
    if (!valid()) return false;
    input.dispatchEvent(new Event('change', {bubbles:true}));
    if (!valid()) return false;
  }
  if (![[user,username],[pass,password]].every(([input,value])=>{
    if (!input) return true;
    const normalized=document.createElement('input');normalized.type=input.type;normalized.multiple=input.multiple;setter.call(normalized,value);
    return input.value===normalized.value;
  })) return false;
  if (submit) {
    // Formless SPAs need a separately authorized, snapshotted control action.
    if (!form) return 'unsupported';
    const controls = [...form.elements].filter(el => (el instanceof HTMLButtonElement || el instanceof HTMLInputElement) && el.type === 'submit');
    const usable = controls.filter(el => { const rect = el.getBoundingClientRect(); return rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight
      && el.contains(document.elementFromPoint(rect.left + rect.width/2, rect.top + rect.height/2)) && safeSubmitter(el) && el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})
      && !el.closest('[inert],[hidden],[aria-hidden="true"]'); });
    // Never bypass a custom login button by blindly posting its enclosing form.
    if (controls.length && usable.length !== 1) return 'unsupported';
    if (!controls.length && form.querySelector('button,[role="button"],input[type="button"],input[type="image"]')) return 'unsupported';
    if (!valid()) return false;
    return submitLoginForm(form, usable[0]);
  }
  return true;
}`;

export async function fillBrowserVault(options: {
  cdp: PrivateBrowserChannel;
  sessionId: string;
  request: BrowserVaultRequest;
  expectedLoaderId?: string;
  resolve: () => Promise<BrowserVaultLogin>;
  quarantine: (value: BrowserVaultQuarantine) => Promise<void>;
  signal?: AbortSignal;
}): Promise<{ status: "submitted" | "filled"; submission?: "action_required" } | { status: "outcome_unknown"; next_action: "inspect_before_retry" }> {
  try {
    const { cdp, request } = options;
    const checkAbort = () => { if (options.signal?.aborted) throw new Error(); };
    checkAbort();
    const target = await cdp.send("Target.getTargetInfo", { targetId: request.target_id });
    if (target?.targetInfo?.type !== "page" || new URL(target.targetInfo.url).origin !== request.expected_origin) throw new Error();
    const attached = await attachPrivateTarget(cdp, request.target_id);
    const sid = attached?.sessionId;
    if (typeof sid !== "string") throw new Error();
    const tree = await cdp.send("Page.getFrameTree", {}, sid);
    const frame = tree?.frameTree?.frame;
    if (!frame || frame.parentId || typeof frame.id !== "string" || typeof frame.loaderId !== "string"
      || new URL(frame.url).origin !== request.expected_origin
      || (options.expectedLoaderId !== undefined && frame.loaderId !== options.expectedLoaderId)) throw new Error();
    const world = await cdp.send("Page.createIsolatedWorld", { frameId: frame.id, worldName: "nanocodex-vault", grantUniveralAccess: false }, sid);
    if (!Number.isInteger(world?.executionContextId)) throw new Error();
    if (options.expectedLoaderId !== undefined) {
      // Creating a world can race navigation in the same top-level frame. The
      // world must have been created before a second matching document check;
      // subsequent navigation destroys that world instead of retargeting input.
      const current = (await cdp.send("Page.getFrameTree", {}, sid))?.frameTree?.frame;
      if (!current || current.parentId || current.id !== frame.id
        || current.loaderId !== options.expectedLoaderId
        || new URL(current.url).origin !== request.expected_origin) throw new Error();
    }
    const login = await options.resolve();
    checkAbort();
    if ((request.username_selector && !login.username) || (request.password_selector && !login.password)) throw new Error();
    // Persist before any secret reaches the browser, including ambiguous failures.
    await options.quarantine({ sessionId: options.sessionId, targetId: request.target_id, loaderId: frame.loaderId, origin: request.expected_origin, vaultId: request.vault_id });
    checkAbort();
    let result;
    try {
      result = await cdp.send("Runtime.callFunctionOn", {
        executionContextId: world.executionContextId,
        functionDeclaration: BROWSER_VAULT_FILL_FUNCTION,
        arguments: [request.expected_origin, request.username_selector ?? null, request.password_selector ?? null, request.username_selector ? login.username : null, request.password_selector ? login.password : null, request.submit].map(value => ({ value })),
        returnByValue: true,
        silent: true,
      }, sid);
    } catch {
      // Submission can navigate and destroy the execution context before CDP
      // returns. Never claim failure or replay a possibly completed login.
      return { status: "outcome_unknown", next_action: "inspect_before_retry" };
    }
    if (result?.exceptionDetails) return { status: "outcome_unknown", next_action: "inspect_before_retry" };
    if (result?.result?.value === "unsupported") return { status: "filled", submission: "action_required" };
    if (result?.result?.value !== true) throw new Error();
    return { status: request.submit ? "submitted" : "filled" };
  } catch { throw new Error("Vault login could not be filled safely"); }
}


export type BrowserVaultIdentity = Pick<BrowserVaultRequest, "vault_id" | "expected_origin" | "target_id">;
export type BrowserVaultStatus = "login_form" | "username_form" | "password_form" | "otp_form" | "challenge" | "unknown" | "destination_changed";
export type BrowserVaultInspection = {
  status: BrowserVaultStatus; username_selector?: string; password_selector?: string; otp_selector?: string;
};
export type BrowserVaultSnapshot = BrowserVaultInspection & {
  snapshot_id: string; title: string; text: string;
  elements: { ref: string; role: "link" | "button" | "input" | "textarea" | "select" | "checkbox" | "radio"; text: string;
    options?: { index: number; label: string }[]; checked?: boolean; native_input: boolean; input_type?: string }[];
};
const USERNAME_SELECTOR = 'input:not([type]):not([autocomplete="one-time-code"]),input[type="text"]:not([autocomplete="one-time-code"]),input[type="email"]';
const PASSWORD_SELECTOR = 'input[type="password"]';
const OTP_SELECTOR = 'input[autocomplete="one-time-code"],input[name="otp"],input[name="code"],input[name="verification_code"]';

// Native sheets bind form semantics and control state, not unrelated prose such
// as resend countdowns. Keep the stricter whole-form snapshot for agent actions.
// Fieldsets contribute attributes only: their descendant controls are included
// separately, so changing help text inside a fieldset is harmless too.
export const NATIVE_FORM_STATE = `form => form ? JSON.stringify([
  [form.cloneNode(false).outerHTML, form.action, form.method, form.target, form.enctype, form.noValidate],
  [...form.elements].map(e => e instanceof HTMLFieldSetElement ? [e.cloneNode(false).outerHTML] : fieldState(e))
]) : null`;

/** Fixed isolated-world code. Only bounded visible labels and option indices leave
 * the browser. Values and captured DOM/form state remain private. User authority
 * is enforced by the host; control labels never grant or restrict that authority.
 */
export const BROWSER_VAULT_CONTINUATION_FUNCTION = `function(origin, mode, snapshotId, ref, url, selectors, payload) {
  if (window !== window.top || location.origin !== origin || location.protocol !== 'https:') return null;
  ${VAULT_FORM_SUBMISSION}
  const visible = (el, readingText = false) => el instanceof Element && el.isConnected && el.getRootNode() === document
    && !el.closest('[inert],[hidden],script,style,noscript,template' + (readingText ? ',textarea,select' : ',[aria-hidden="true"]'))
    && el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) && el.getClientRects().length > 0;
  const enabled = el => !el.disabled && !el.matches(':disabled') && el.getAttribute('aria-disabled') !== 'true';
  const safeUrl = value => { try { const u = new URL(value, location.href); return u.protocol === 'https:' && u.origin === origin && !u.username && !u.password ? u.href : null; } catch { return null; } };
  const safeForm = form => form instanceof HTMLFormElement && form.isConnected && form.getRootNode() === document
    && ['get','post'].includes(form.method.toLowerCase()) && (!form.target || form.target === '_self') && !!safeUrl(form.action);
  const associatedForm = el => el.form || el.closest('form');
  const readable = root => {
    const chunks = [], walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node, visited = 0, length = 0;
    while ((node = walker.nextNode()) && ++visited <= 10000) {
      const parent = node.parentElement;
      if (!parent || !visible(parent, true) || parent.closest('input,option')) continue;
      const value = node.textContent || '';
      if (!value.trim() || value.length > 8192) continue;
      if (length + value.length > 32768) break;
      chunks.push(value); length += value.length;
    }
    return chunks.join(' ');
  };
  const label = el => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.length <= 8192) return aria;
    const ids = (el.getAttribute('aria-labelledby') || '').trim().split(/\\s+/).filter(Boolean).slice(0, 20);
    const named = ids.map(id => document.getElementById(id)).filter(node => node && visible(node, true)).map(readable).join(' ');
    if (named) return named;
    const labels = el.labels ? [...el.labels].filter(node => visible(node, true)).map(readable).join(' ') : '';
    if (labels) return labels;
    if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
      const placeholder = el.getAttribute('placeholder') || '';
      if (placeholder && placeholder.length <= 8192) return placeholder;
      // Submit inputs render their value as a caption, but input values stay
      // private. A fixed role label still lets the agent choose the submit ref.
      return el instanceof HTMLInputElement && el.type === 'submit' ? 'Submit form' : '';
    }
    return el instanceof HTMLSelectElement ? '' : readable(el);
  };
  const sensitive = el => {
    if (el instanceof HTMLInputElement && el.type === 'password') return true;
    const hints = [el.id, el.getAttribute('name'), el.getAttribute('autocomplete'), el.getAttribute('inputmode'), label(el)].join(' ');
    return /password|passcode|passwd|one.time.code|verification|security.code|totp|2fa|mfa|\\botp\\b|\\bpin\\b|\\bcode\\b|\\bcc[-_ ]|card|ccnum|cvc|cvv|expir(?:y|ation)|payment|iban|routing|sort.code|swift|bank|account.number|social.security|\\bssn\\b|tax.id|passport|national.id|driver.?s?.license|secret|token|api.key|private.key|recovery.code|sensitive/i.test(hints);
  };
  const role = el => {
    if (el instanceof HTMLAnchorElement) return 'link';
    if (el instanceof HTMLTextAreaElement) return 'textarea';
    if (el instanceof HTMLSelectElement) return 'select';
    if (el instanceof HTMLInputElement) return ['checkbox','radio'].includes(el.type) ? el.type
      : ['button','submit','reset'].includes(el.type) ? 'button' : el.type !== 'hidden' && el.type !== 'image' && el.type !== 'file' ? 'input' : null;
    return el instanceof HTMLButtonElement || (el instanceof HTMLElement && el.getAttribute('role') === 'button'
      && !el.closest('a,button,label,summary')) ? 'button' : null;
  };
  const safeControl = el => {
    const kind = role(el), form = associatedForm(el);
    if (!kind || !visible(el) || !enabled(el) || (form && !safeForm(form))) return false;
    if (kind === 'link') return !!safeUrl(el.href) && (!el.target || el.target === '_self') && !el.hasAttribute('download');
    return !['formaction','formmethod','formtarget','formenctype','formnovalidate'].some(a => el.hasAttribute(a));
  };
  const usable = selector => {
    const nodes = document.querySelectorAll(selector);
    return nodes.length === 1 && nodes[0] instanceof HTMLInputElement && visible(nodes[0])
      && enabled(nodes[0]) && !nodes[0].readOnly && (!nodes[0].form || safeLoginForm(nodes[0].form));
  };
  const challenge = [...document.querySelectorAll('iframe,[id],[class]')].slice(0, 5000).some(el =>
    visible(el) && /captcha|turnstile|challenge-platform/i.test([el.id, typeof el.className === 'string' ? el.className : '', el instanceof HTMLIFrameElement ? el.src : ''].join(' ')));
  const flags = selectors.map(usable);
  const status = challenge ? 'challenge' : flags[2] ? 'otp_form' : flags[0] && flags[1] ? 'login_form' : flags[1] ? 'password_form' : flags[0] ? 'username_form' : 'unknown';
  if (mode === 'status') return {status, flags};
  if (mode === 'navigate') {
    const destination = safeUrl(url);
    if (!destination) return false;
    delete globalThis.__nanocodexVaultSnapshot;
    location.assign(destination);
    return true;
  }
  // Live values are retained only in this isolated world for TOCTOU checks. They
  // are never included in the return value, labels, diagnostics or CDP output.
  const fieldState = el => [el.outerHTML, 'value' in el ? el.value : null,
    'checked' in el ? el.checked : null, el instanceof HTMLSelectElement ? [...el.options].map(o => o.selected) : null];
  const formState = form => form ? JSON.stringify([form.outerHTML, [...form.elements].map(fieldState)]) : null;
  const nativeFormState = ${NATIVE_FORM_STATE};
  const nativeInput = el => !el.readOnly && (
    el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement && !el.multiple && el.options.length <= 200
      && [...el.options].some(o => !o.disabled && !o.hidden && !o.closest('optgroup[disabled],optgroup[hidden]'))
    || el instanceof HTMLInputElement && ['text','search','email','url','tel','number','password','checkbox'].includes(el.type));
  if (mode === 'request_input') {
    const snapshot = globalThis.__nanocodexVaultSnapshot;
    if (!snapshot || snapshot.id !== snapshotId || snapshot.document !== document || snapshot.href !== location.href) return false;
    if (challenge) return false;
    const entries = payload.fields.map(field => ({...snapshot.nodes.get(field.ref), formState:snapshot.nodes.get(field.ref)?.nativeFormState, requestedLabel:field.label}));
    const valid = () => snapshot.document === document && snapshot.href === location.href && entries.every(entry => entry.el && safeControl(entry.el) && nativeInput(entry.el)
      && label(entry.el) === entry.label && JSON.stringify(fieldState(entry.el)) === entry.state
      && associatedForm(entry.el) === entry.form && nativeFormState(entry.form) === entry.formState);
    if (!valid()) return false;
    // Retain actual nodes only in this isolated world. Never expose selectors or values.
    globalThis.__nanocodexNativeSelection = {id:ref, document, href:location.href, entries, reason:payload.reason, valid};
    return true;
  }
  if (['click','fill','select','check'].includes(mode)) {
    const snapshot = globalThis.__nanocodexVaultSnapshot;
    if (!snapshot) return 'snapshot_missing';
    if (snapshot.id !== snapshotId) return 'stale_ref';
    if (snapshot.document !== document || snapshot.href !== location.href) return 'document_changed';
    const entry = snapshot.nodes.get(ref), el = entry && entry.el;
    delete globalThis.__nanocodexVaultSnapshot;
    if (challenge) return 'challenge_detected';
    if (!el) return 'stale_ref';
    if (!visible(el) || !enabled(el)) return 'element_not_visible';
    const unchanged = () => location.origin === origin && el.isConnected && el.getRootNode() === document
      && label(el) === entry.label && JSON.stringify(fieldState(el)) === entry.state
      && associatedForm(el) === entry.form && formState(entry.form) === entry.formState;
    if (!unchanged()) return 'changed_element';
    el.scrollIntoView({block:'center', inline:'center', behavior:'instant'});
    if (!unchanged()) return 'changed_element';
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0 || rect.left < 0 || rect.top < 0 || rect.right > innerWidth || rect.bottom > innerHeight) return 'outside_viewport';
    if (!el.contains(document.elementFromPoint(rect.left + rect.width/2, rect.top + rect.height/2))) return 'occluded';
    if (!safeControl(el)) return 'unsafe_destination';
    const kind = role(el);
    if (['fill','select'].includes(mode) && sensitive(el)) return 'sensitive_field';
    // Enforce native destinations even when page handlers call form.submit(). A
    // credential-bearing GET form is JS-only. Page JS on the authorized origin
    // can still make its own requests, as it could during private login.
    const credentialGet = [...document.forms].some(form => form.method.toLowerCase() !== 'post'
      && [...form.elements].some(field => (field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement) && sensitive(field)));
    if (!protectNativeForms(credentialGet)) return 'unsafe_destination';
    const guard = event => {
      const form = event.target, submitter = event.submitter;
      if (!safeForm(form) || (submitter && (submitter.form !== form || !safeControl(submitter)))
        || (form.method.toLowerCase() !== 'post' && [...form.elements].some(sensitive))) event.preventDefault();
    };
    window.addEventListener('submit', guard);
    try {
      if (mode === 'fill') {
        if (!['input','textarea'].includes(kind) || el.readOnly
          || (kind === 'input' && !['text','email','tel','url','search','number','date','datetime-local','month','week','time'].includes(el.type))
          || typeof payload !== 'string' || payload.length > 8192) return 'unsupported_element';
        const prototype = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value').set.call(el, payload);
        el.dispatchEvent(new Event('input', {bubbles:true}));
        if (!el.isConnected || associatedForm(el) !== entry.form || (entry.form && !safeForm(entry.form))) return 'changed_element';
        el.dispatchEvent(new Event('change', {bubbles:true}));
        return true;
      }
      if (mode === 'select') {
        if (kind !== 'select' || el.multiple || !Number.isInteger(payload) || payload < 0 || payload >= el.options.length) return 'unsupported_element';
        const option = el.options[payload];
        if (option.disabled || option.parentElement instanceof HTMLOptGroupElement && option.parentElement.disabled || option.hidden || option.closest('optgroup[hidden]')) return 'unsupported_element';
        Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'selectedIndex').set.call(el, payload);
        el.dispatchEvent(new Event('input', {bubbles:true}));
        if (!el.isConnected || associatedForm(el) !== entry.form || (entry.form && !safeForm(entry.form))) return 'changed_element';
        el.dispatchEvent(new Event('change', {bubbles:true}));
        return true;
      }
      if (mode === 'check') {
        if (!['checkbox','radio'].includes(kind) || typeof payload !== 'boolean' || kind === 'radio' && !payload) return 'unsupported_element';
        if (el.checked !== payload) HTMLElement.prototype.click.call(el);
        return el.checked === payload;
      }
      if (!['link','button'].includes(kind)) return 'unsupported_element';
      HTMLElement.prototype.click.call(el);
      return true;
    } finally { window.removeEventListener('submit', guard); }
  }
  if (mode !== 'snapshot') return null;
  const nodes = new Map(), elements = [];
  for (const el of [...document.querySelectorAll('a[href],button,input,textarea,select,[role="button"]')].slice(0, 2000)) {
    if (elements.length >= 200) break;
    if (!safeControl(el)) continue;
    const kind = role(el), form = associatedForm(el), text = label(el);
    if (form && form.elements.length > 2000 || kind === 'select' && el.options.length > 200) continue;
    const key = 'e' + (elements.length + 1);
    nodes.set(key, {el, label:text, state:JSON.stringify(fieldState(el)), form, formState:formState(form), nativeFormState:nativeFormState(form)});
    // Never read submit input values as labels; options expose indices and labels only.
    const options = kind === 'select' ? [...el.options].flatMap((option, index) => option.hidden || option.closest('optgroup[hidden]') ? [] : [{index, label:option.label.length <= 8192 ? option.label : ''}]) : undefined;
    elements.push({ref:key, role:kind, text, native_input:nativeInput(el), ...(nativeInput(el) ? {input_type:el instanceof HTMLTextAreaElement ? 'text' : el instanceof HTMLSelectElement ? 'select' : el.type} : {}), ...(options ? {options} : {}), ...(['checkbox','radio'].includes(kind) ? {checked:el.checked} : {})});
  }
  globalThis.__nanocodexVaultSnapshot = {id:snapshotId, document, href:location.href, nodes};
  return {status, flags, snapshot_id:snapshotId, title:document.title.length <= 8192 ? document.title : '', text:readable(document.body || document.documentElement), elements};
}`;

function validateIdentity(request: BrowserVaultIdentity) {
  if (!request || !isBrowserVaultOrigin(request.expected_origin)
    || !/^[A-Za-z0-9_-]{22,64}$/.test(request.vault_id)
    || !/^[A-Za-z0-9_-]{1,128}$/.test(request.target_id)) throw new Error();
}
async function privateWorld(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity) {
  validateIdentity(request);
  const target = await cdp.send("Target.getTargetInfo", { targetId: request.target_id });
  if (target?.targetInfo?.type !== "page") throw new Error();
  if (new URL(target.targetInfo.url).origin !== request.expected_origin) return null;
  const attached = await attachPrivateTarget(cdp, request.target_id);
  if (typeof attached?.sessionId !== "string") throw new Error();
  const tree = await cdp.send("Page.getFrameTree", {}, attached.sessionId);
  const frame = tree?.frameTree?.frame;
  if (!frame || frame.parentId || typeof frame.id !== "string" || typeof frame.loaderId !== "string" || new URL(frame.url).origin !== request.expected_origin) return null;
  const world = await cdp.send("Page.createIsolatedWorld", { frameId: frame.id, worldName: "nanocodex-vault-continuation", grantUniveralAccess: false }, attached.sessionId);
  if (!Number.isInteger(world?.executionContextId)) throw new Error();
  // frame.id survives navigation; never pair an old loader with a new world's
  // execution context. Later navigation invalidates this context and fails closed.
  const verifiedTree = await cdp.send("Page.getFrameTree", {}, attached.sessionId);
  const verifiedFrame = verifiedTree?.frameTree?.frame;
  if (!verifiedFrame || verifiedFrame.parentId || verifiedFrame.id !== frame.id
    || verifiedFrame.loaderId !== frame.loaderId || new URL(verifiedFrame.url).origin !== request.expected_origin) throw new Error();
  return { sessionId: attached.sessionId as string, executionContextId: world.executionContextId as number, loaderId: frame.loaderId as string };
}
async function continuation(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity, mode: string, snapshotId = "", ref = "", url = "", payload?: string | number | boolean | BrowserVaultInputSelection) {
  const world = await privateWorld(cdp, request);
  if (!world) return null;
  const result = await cdp.send("Runtime.callFunctionOn", {
    executionContextId: world.executionContextId, functionDeclaration: BROWSER_VAULT_CONTINUATION_FUNCTION,
    arguments: [request.expected_origin, mode, snapshotId, ref, url, [USERNAME_SELECTOR, PASSWORD_SELECTOR, OTP_SELECTOR], payload ?? null].map(value => ({ value })),
    returnByValue: true, silent: true,
  }, world.sessionId);
  if (result?.exceptionDetails) throw new Error();
  return result?.result?.value;
}
function inspection(value: any): BrowserVaultInspection {
  if (!value) return { status: "destination_changed" };
  if (!["login_form", "username_form", "password_form", "otp_form", "challenge", "unknown"].includes(value.status)
    || !Array.isArray(value.flags) || value.flags.length !== 3 || value.flags.some((v: unknown) => typeof v !== "boolean")) throw new Error();
  return { status: value.status,
    ...(value.flags[0] ? { username_selector: USERNAME_SELECTOR } : {}),
    ...(value.flags[1] ? { password_selector: PASSWORD_SELECTOR } : {}),
    ...(value.flags[2] ? { otp_selector: OTP_SELECTOR } : {}),
  };
}

/** Absence of supported inputs is unknown, never evidence of authentication. */
export async function inspectBrowserVault(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity): Promise<BrowserVaultInspection> {
  try { return inspection(await continuation(cdp, request, "status")); }
  catch { throw new Error("Private login status is unavailable"); }
}

/** All redaction runs on the host. Caller must supply every credential/code used in
 * this quarantined session when available. Numeric OTP masking also survives
 * rehydration without exact codes. This is defense in depth, not protection from a site
 * deliberately transforming/exfiltrating a credential it has already received.
 */
export function sanitizeBrowserVaultText(value: string, secrets: readonly string[], limit: number): string {
  const variants = new Set<string>();
  for (const secret of secrets) {
    if (!secret) continue;
    variants.add(secret);
    const encodings = [secret, encodeURIComponent(secret), encodeURI(secret), new URLSearchParams({ v: secret }).toString().slice(2)];
    for (const encoded of encodings) {
      variants.add(encoded); variants.add(encodeURIComponent(encoded));
      variants.add(encoded.replace(/%[0-9A-F]{2}/g, match => match.toLowerCase()));
    }
    variants.add(secret.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!));
    variants.add([...secret].map(c => `&#${c.codePointAt(0)};`).join(''));
    variants.add([...secret].map(c => `&#x${c.codePointAt(0)!.toString(16)};`).join(''));
    try { variants.add(btoa(unescape(encodeURIComponent(secret)))); } catch { /* Invalid Unicode has no UTF-8 variant. */ }
  }
  let safe = value;
  for (const variant of [...variants].sort((a, b) => b.length - a.length)) {
    const escaped = [...variant].map(c => c.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s*');
    safe = safe.replace(new RegExp(escaped, 'giu'), '[redacted]');
  }
  // Runtime limits verification intake to numeric codes. Conservative generic
  // masking survives worker rehydration when the exact prior code is unavailable.
  // This also masks some dates/prices; retain that conservative behavior until
  // exact secret retention can safely replace generic numeric masking.
  // Normalize common accidental echo encodings before masking; arbitrary site
  // transformations are outside this defense-in-depth boundary.
  for (let round = 0; round < 3; round++) {
    safe = safe.replace(/(?:%[0-9a-f]{2})+/gi, encoded => { try { return decodeURIComponent(encoded); } catch { return encoded; } })
      .replace(/&amp;/gi, '&')
      .replace(/&#(x[0-9a-f]+|[0-9]+);/gi, (encoded, digits: string) => {
        const point = digits[0]!.toLowerCase() === 'x' ? parseInt(digits.slice(1), 16) : parseInt(digits, 10);
        return point > 0 && point <= 0x10ffff ? String.fromCodePoint(point) : encoded;
      });
  }
  // Decoding mixed encodings may reconstruct a known credential; redact again.
  for (const variant of [...variants].sort((a, b) => b.length - a.length)) {
    const escaped = [...variant].map(c => c.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s*');
    safe = safe.replace(new RegExp(escaped, 'giu'), '[redacted]');
  }
  for (const secret of secrets) {
    const digits = secret.replace(/[\s/.-]/g, '');
    if (/^[0-9]{3,32}$/.test(digits)) safe = safe.replace(new RegExp([...digits].join('[\\s/.-]*'), 'g'), '[redacted]');
  }
  safe = safe.replace(/\b[A-Za-z0-9+/_-]{6,16}={0,2}/g, encoded => {
    try { return /^[0-9]{4,10}$/.test(atob(encoded.replace(/-/g, '+').replace(/_/g, '/'))) ? '[redacted]' : encoded; }
    catch { return encoded; }
  });
  safe = safe.replace(/\b(?:[a-z][a-z0-9+.-]*:\/\/|www\.)[^\s<>"']+/gi, '[url omitted]')
    .replace(/[0-9](?:[\s-]*[0-9]){3,}/g, '[redacted]');
  return safe.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, '').replace(/\s+/g, ' ').trim().slice(0, limit);
}

export async function snapshotBrowserVault(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity, secrets: readonly string[]): Promise<BrowserVaultSnapshot> {
  try {
    if (!Array.isArray(secrets) || secrets.some(s => typeof s !== "string")) throw new Error();
    const id = crypto.randomUUID();
    const value = await continuation(cdp, request, "snapshot", id);
    const status = inspection(value);
    if (!value) return { ...status, snapshot_id: "", title: "", text: "", elements: [] };
    if (value.snapshot_id !== id || typeof value.title !== "string" || value.title.length > 8192
      || typeof value.text !== "string" || value.text.length > 65536 || !Array.isArray(value.elements) || value.elements.length > 200) throw new Error();
    const clean = (text: string, limit: number) => sanitizeBrowserVaultText(text, secrets, limit);
    const elements = value.elements.map((el: any) => {
      if (!el || !/^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/.test(el.ref)
        || !["link", "button", "input", "textarea", "select", "checkbox", "radio"].includes(el.role)
        || typeof el.text !== "string" || el.text.length > 65536) throw new Error();
      const options = el.role === "select" ? el.options : undefined;
      if (el.role === "select" && (!Array.isArray(options) || options.length > 200 || options.some((option: any, index: number) =>
        !option || !Number.isInteger(option.index) || option.index < 0 || option.index >= 200 || (index > 0 && option.index <= options[index - 1].index)
        || typeof option.label !== "string" || option.label.length > 8192))) throw new Error();
      const checkable = el.role === "checkbox" || el.role === "radio";
      if (checkable && typeof el.checked !== "boolean") throw new Error();
      return { ref: el.ref as string, role: el.role as BrowserVaultSnapshot["elements"][number]["role"], text: clean(el.text, 256), native_input:el.native_input === true,
        ...(el.native_input === true && ["text","search","email","url","tel","number","password","select","checkbox"].includes(el.input_type) ? {input_type:el.input_type as string} : {}),
        ...(options ? { options: options.map((option: {index: number; label: string}) => ({index: option.index, label: clean(option.label, 256)})) } : {}),
        ...(checkable ? { checked: el.checked as boolean } : {}) };
    });
    return { ...status, snapshot_id: id, title: clean(value.title, 256), text: clean(value.text, 12000), elements };
  } catch { throw new Error("Private page snapshot is unavailable"); }
}

export type BrowserVaultInputSelection = { fields: {ref: string; label?: string}[]; reason?: string };
export const browserVaultInputSelectionProperties = {
  snapshot_id:{type:"string"},
  fields:{type:"array",minItems:1,maxItems:32,items:{type:"object",additionalProperties:false,
    properties:{ref:{type:"string"},label:{type:"string",maxLength:160}},required:["ref"]}},
  reason:{type:"string",maxLength:500},
};
export function parseBrowserVaultInputSelection(v: Record<string,unknown>, secrets: readonly string[]): BrowserVaultInputSelection | undefined {
  if (v.reason !== undefined && (typeof v.reason !== "string" || !v.reason.trim() || v.reason.length > 500)) {
    throw new Error("Provide a non-empty input reason of at most 500 characters");
  }
  // A reason describes either native input or browser takeover. Only actual
  // field selection requires a snapshot; partial selections must still fail.
  if (v.snapshot_id === undefined && v.fields === undefined) return undefined;
  if (typeof v.snapshot_id !== "string" || !/^[0-9a-f-]{36}$/i.test(v.snapshot_id)
    || !Array.isArray(v.fields) || !v.fields.length || v.fields.length > 32) throw new Error("Provide a current snapshot_id and native input refs");
  const refs = new Set<string>();
  const fields = v.fields.map(field => {
    if (!field || typeof field !== "object" || Array.isArray(field) || Object.keys(field).some(k => !["ref","label"].includes(k))
      || typeof field.ref !== "string" || !/^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/.test(field.ref) || refs.has(field.ref)
      || (field.label !== undefined && (typeof field.label !== "string" || !field.label.trim() || field.label.length > 160))) throw new Error("Invalid native input field selection");
    refs.add(field.ref);
    return {ref:field.ref, ...(field.label !== undefined ? {label:sanitizeBrowserVaultText(field.label,secrets,160)} : {})};
  });
  return {fields, ...(typeof v.reason === "string" ? {reason:sanitizeBrowserVaultText(v.reason,secrets,500)} : {})};
}
/** Bind an agent's field choice to the exact redacted snapshot, without any input. */
export async function selectBrowserVaultInput(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity,
  snapshotId: string, selection: BrowserVaultInputSelection): Promise<string | undefined> {
  const id = crypto.randomUUID();
  try { return await continuation(cdp, request, "request_input", snapshotId, id, "", selection) === true ? id : undefined; }
  catch { return undefined; }
}

// Only these fixed host-owned categories may cross the private diagnostic boundary.
const CLICK_FAILURES = ["snapshot_missing", "stale_ref", "document_changed", "challenge_detected",
  "element_not_visible", "changed_element", "outside_viewport", "occluded", "unsafe_destination", "unsupported_element", "sensitive_field"] as const;
type BrowserVaultClickFailure = typeof CLICK_FAILURES[number];
export class BrowserVaultActionRejected extends Error {
  constructor(reason: BrowserVaultClickFailure) {
    super(`Private browser action could not be completed safely (${reason})`);
    this.name = "BrowserVaultActionRejected";
  }
}

export type BrowserVaultAction = { action: "click"; snapshot_id: string; ref: string }
  | { action: "fill"; snapshot_id: string; ref: string; text: string }
  | { action: "select"; snapshot_id: string; ref: string; option_index: number }
  | { action: "check"; snapshot_id: string; ref: string; checked: boolean }
  | { action: "navigate"; url: string };
export async function actBrowserVault(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity, action: BrowserVaultAction): Promise<{ status: "navigation_requested" | "action_requested" }> {
  try {
    if (action.action === "navigate") {
      const destination = new URL(action.url);
      if (action.url.length > 8192 || destination.origin !== request.expected_origin || destination.username || destination.password) throw new Error();
      if (await continuation(cdp, request, "navigate", "", "", destination.href) !== true) throw new Error();
      return { status: "navigation_requested" };
    }
    if (!["click", "fill", "select", "check"].includes(action.action) || !/^[0-9a-f-]{36}$/.test(action.snapshot_id)
      || !/^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/.test(action.ref)) throw new Error();
    if (action.action === "fill" && (typeof action.text !== "string" || action.text.length > 8192)) throw new Error();
    if (action.action === "select" && (!Number.isInteger(action.option_index) || action.option_index < 0 || action.option_index >= 200)) throw new Error();
    if (action.action === "check" && typeof action.checked !== "boolean") throw new Error();
    const payload = action.action === "fill" ? action.text : action.action === "select" ? action.option_index : action.action === "check" ? action.checked : undefined;
    const result = await continuation(cdp, request, action.action, action.snapshot_id, action.ref, "", payload);
    if (typeof result === "string" && CLICK_FAILURES.includes(result as BrowserVaultClickFailure)) throw new BrowserVaultActionRejected(result as BrowserVaultClickFailure);
    if (result !== true) throw new Error();
    return { status: "action_requested" };
  } catch (error) {
    if (error instanceof BrowserVaultActionRejected) throw error;
    throw new Error("Private browser action could not be completed safely");
  }
}

export const BROWSER_VAULT_OTP_FUNCTION = `function(origin, selector, code, submit) {
  if (window !== window.top || location.origin !== origin || location.protocol !== 'https:') return false;
  const nodes = document.querySelectorAll(selector);
  if (nodes.length !== 1) return false;
  const input = nodes[0];
  if (!(input instanceof HTMLInputElement) || !['text','tel','number','password'].includes(input.type)
    || input.disabled || input.readOnly || !input.isConnected || input.getRootNode() !== document
    || input.closest('[inert],[hidden],[aria-hidden="true"]') || !input.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})) return false;
  const rect = input.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0 || rect.left < 0 || rect.top < 0 || rect.right > innerWidth || rect.bottom > innerHeight
    || document.elementFromPoint(rect.left + rect.width/2, rect.top + rect.height/2) !== input) return false;
  const form = input.form;
  if (!form || form.method.toLowerCase() !== 'post' || (form.target && form.target !== '_self')) return false;
  const action = new URL(form.action, location.href);
  if (action.origin !== origin || action.username || action.password) return false;
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, code);
  if (submit) HTMLFormElement.prototype.submit.call(form);
  return true;
}`;

/** Host-only code resolver. Runtime must retain quarantine and ensure the same
 * user-authorized Vault item/target/origin before calling; no code enters output.
 */
export async function fillBrowserVaultOtp(options: {
  cdp: PrivateBrowserChannel;
  request: BrowserVaultIdentity & { otp_selector: string; expected_loader_id?: string };
  resolve: () => Promise<string>;
  submit: boolean;
  signal?: AbortSignal;
}): Promise<{ status: "filled" | "submitted" }> {
  try {
    if (typeof options.submit !== "boolean" || typeof options.request.otp_selector !== "string"
      || !options.request.otp_selector.trim() || options.request.otp_selector.length > 512 || options.signal?.aborted) throw new Error();
    const world = await privateWorld(options.cdp, options.request);
    if (!world || (options.request.expected_loader_id !== undefined && world.loaderId !== options.request.expected_loader_id)) throw new Error();
    const code = await options.resolve();
    if (typeof code !== "string" || !/^[0-9]{4,10}$/.test(code) || options.signal?.aborted) throw new Error();
    const result = await options.cdp.send("Runtime.callFunctionOn", {
      executionContextId: world.executionContextId, functionDeclaration: BROWSER_VAULT_OTP_FUNCTION,
      arguments: [options.request.expected_origin, options.request.otp_selector, code, options.submit].map(value => ({ value })), returnByValue: true, silent: true,
    }, world.sessionId);
    if (result?.exceptionDetails || result?.result?.value !== true) throw new Error();
    return { status: options.submit ? "submitted" : "filled" };
  } catch { throw new Error("Private verification code could not be filled safely"); }
}

/** Host-only binding for one-use verification challenges; never expose loader IDs. */
export async function captureBrowserVaultBinding(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity): Promise<{ loaderId: string; otp_selector: string }> {
  try {
    const world = await privateWorld(cdp, request);
    if (!world) throw new Error();
    const result = await cdp.send("Runtime.callFunctionOn", {
      executionContextId: world.executionContextId, functionDeclaration: BROWSER_VAULT_CONTINUATION_FUNCTION,
      arguments: [request.expected_origin, "status", "", "", "", [USERNAME_SELECTOR, PASSWORD_SELECTOR, OTP_SELECTOR]].map(value => ({ value })),
      returnByValue: true, silent: true,
    }, world.sessionId);
    if (result?.exceptionDetails) throw new Error();
    const status = inspection(result?.result?.value);
    if (status.status !== "otp_form" || !status.otp_selector) throw new Error();
    return { loaderId: world.loaderId, otp_selector: status.otp_selector };
  } catch { throw new Error("Private verification document is unavailable"); }
}

/** Host-only general document binding for human takeover, including CAPTCHA and
 * custom forms. Does not claim authentication or expose any page data. */
export async function captureBrowserVaultDocumentBinding(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity): Promise<{ loaderId: string }> {
  try {
    const world = await privateWorld(cdp, request);
    if (!world) throw new Error();
    return { loaderId: world.loaderId };
  } catch { throw new Error("Private browser document is unavailable"); }
}

/** Validate the exact password selector before asking the user for private input. */
export async function captureBrowserPasswordBinding(cdp: PrivateBrowserChannel, request: BrowserVaultRequest): Promise<{loaderId: string}> {
  try {
    const world = await privateWorld(cdp, request);
    if (!world || !request.password_selector) throw new Error();
    const result = await cdp.send("Runtime.callFunctionOn", {
      executionContextId:world.executionContextId, functionDeclaration:BROWSER_VAULT_FILL_FUNCTION,
      arguments:[request.expected_origin,null,request.password_selector,null,null,false,true].map(value => ({value})),
      returnByValue:true,silent:true,
    },world.sessionId);
    if (result?.exceptionDetails || result?.result?.value !== true) throw new Error();
    return {loaderId:world.loaderId};
  } catch { throw new Error("Supported password form is unavailable"); }
}

export type SecureFormField = {id:string; kind:'password'|'card_number'|'card_expiry'|'card_cvc'|'sensitive_text'; selector:string; label?:string};
export function parseSecureFormFields(value:unknown): SecureFormField[] {
  if (!Array.isArray(value) || !value.length || value.length > 8) throw new Error('Invalid secure fields');
  const ids = new Set(), selectors = new Set();
  for (const f of value) {
    if (!f || typeof f !== 'object' || Array.isArray(f) || Object.keys(f).some(k => !['id','kind','selector','label'].includes(k))
      || typeof f.id !== 'string' || !/^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(f.id)
      || !['password','card_number','card_expiry','card_cvc','sensitive_text'].includes(f.kind)
      || typeof f.selector !== 'string' || !f.selector.trim() || f.selector.length > 512
      || (f.label !== undefined && (typeof f.label !== 'string' || !f.label.trim() || f.label.length > 80 || /[\u0000-\u001f\u007f]/.test(f.label)))
      || ids.has(f.id) || selectors.has(f.selector)) throw new Error('Invalid secure fields');
    ids.add(f.id); selectors.add(f.selector);
  }
  return value;
}
export function parsePrivateSecureInput(value:unknown): Record<string,unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid secure input');
  const v = value as Record<string,unknown>;
  if(typeof v.request_id !== 'string' || !/^[0-9a-f-]{36}$/i.test(v.request_id))throw new Error('Invalid secure input');
  if(Object.hasOwn(v,'action')) {
    if(Object.keys(v).length!==2 || !['cancel','describe','retry_vault_save'].includes(String(v.action)))throw new Error('Invalid secure input');
    return v;
  }
  if(v.save_to_vault !== undefined)validatePrivateVaultSave(v.save_to_vault);
  if(v.save_details !== undefined){validatePrivateVaultDetails(v.save_details);if(v.save_to_vault!==true)throw new Error('Invalid secure input');}
  if(Object.keys(v).some(k=>!['request_id','value','values','save_to_vault','save_details'].includes(k)) || Object.hasOwn(v,'value')===Object.hasOwn(v,'values'))throw new Error('Invalid secure input');
  const secret = (s:unknown) => typeof s === 'string' && s.length > 0 && s.length <= 4096 && !/[\u0000-\u001f\u007f]/.test(s);
  if (Object.hasOwn(v,'value') && secret(v.value)) return v;
  if (v.values && typeof v.values === 'object' && !Array.isArray(v.values)) {
    const entries = Object.entries(v.values);
    if (entries.length > 0 && entries.length <= 8 && entries.every(([k,s]) => /^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(k) && secret(s))) return v;
  }
  throw new Error('Invalid secure input');
}

/** Read classification hints only, never DOM input values. Bound to the admitted document. */
export async function secureVaultSaveForm(cdp:PrivateBrowserChannel,request:BrowserVaultIdentity,fields:SecureFormField[],loaderId:string):Promise<import('./browser-vault-takeover').BrowserVaultNativeForm> {
  const world=await privateWorld(cdp,request);
  if(!world || world.loaderId!==loaderId)throw new Error('Secure form changed');
  const result=await cdp.send('Runtime.callFunctionOn',{executionContextId:world.executionContextId,returnByValue:true,silent:true,
    functionDeclaration:`function(fields) { return fields.map(f=>{const nodes=document.querySelectorAll(f.selector);const el=nodes.length===1?nodes[0]:null;return el instanceof HTMLInputElement ? {type:el.type,label:(el.getAttribute('aria-label') || [...el.labels||[]].map(l=>l.textContent||'').join(' ')).slice(0,160),autocomplete:el.autocomplete.trim().toLowerCase().replace(/\\s+webauthn$/, '').split(/\\s+/).pop()} : null;}); }`,arguments:[{value:fields.map(({selector})=>({selector}))}]},world.sessionId);
  const hints=result?.result?.value;
  if(result?.exceptionDetails || !Array.isArray(hints) || hints.length!==fields.length)throw new Error('Secure form changed');
  const allowed=['username','email','current-password','new-password','one-time-code','tel','cc-number','cc-exp','cc-exp-month','cc-exp-year','cc-csc','postal-code','address-line1','street-address','address-line2','address-level2','address-level1','country','country-name'];
  return {document_id:loaderId,fields:fields.map((f,i)=>({ref:f.id,label:(hints[i]?.label || f.label || f.id),multiline:false,type:hints[i]?.type==='password'?'password':hints[i]?.type==='email'?'email':'text',...(f.kind==="card_cvc"?{autocomplete:"cc-csc" as const}:allowed.includes(hints[i]?.autocomplete)?{autocomplete:hints[i].autocomplete}:{})}))};
}

// Only native, visible, top-frame input elements in one same-origin POST form.
// No page-provided return values or exception details leave this private boundary.
export const SECURE_FORM_FILL_FUNCTION = `function(origin, fields, values) {
  if (location.origin !== origin || window.top !== window) return false;
  const visible = el => {
    if (!(el instanceof HTMLInputElement) || !el.isConnected || el.disabled || el.matches(':disabled') || el.readOnly
      || el.getRootNode() !== document || el.closest('[inert]')) return false;
    const style = getComputedStyle(el), rect = el.getBoundingClientRect();
    if (style.visibility !== 'visible' || style.display === 'none' || Number(style.opacity) === 0
      || rect.width <= 0 || rect.height <= 0 || rect.left < 0 || rect.top < 0
      || rect.right > innerWidth || rect.bottom > innerHeight) return false;
    if (!el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})) return false;
    return document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2) === el;
  };
  // Never let a model-described card value land in a generic contact/text field.
  const autocomplete = {card_number:'cc-number',card_expiry:'cc-exp',card_cvc:'cc-csc'};
  const matchesKind = (el, kind) => kind === 'password' ? el.type === 'password'
    : ['text','tel','password'].includes(el.type)
      && (!autocomplete[kind] || el.autocomplete.trim().toLowerCase().split(/\\s+/).includes(autocomplete[kind]));
  const inputs = fields.map(f => { const nodes = document.querySelectorAll(f.selector); return nodes.length === 1 ? nodes[0] : null; });
  const form = inputs[0] && inputs[0].form;
  const formAttributes = form && JSON.stringify([...form.attributes].map(a=>[a.name,a.value]));
  const untouched = () => form && JSON.stringify([...form.elements].filter(el=>!inputs.includes(el)).map(el=>[el.outerHTML,el.value,el.checked]));
  const untouchedState = untouched();
  const valid = () => {
    if (JSON.stringify([...form?.attributes||[]].map(a=>[a.name,a.value])) !== formAttributes || untouched() !== untouchedState) return false;
    if (!(form instanceof HTMLFormElement) || form.method.toLowerCase() !== 'post' || (form.target && form.target !== '_self')) return false;
    const action = new URL(form.action, location.href);
    return location.origin === origin && action.origin === origin && !action.username && !action.password && new Set(inputs).size === inputs.length && inputs.every((el,i) => {
      const nodes = document.querySelectorAll(fields[i].selector);
      return nodes.length === 1 && nodes[0] === el && visible(el) && el.form === form && matchesKind(el, fields[i].kind);
    });
  };
  if (!valid()) return false;
  if (values === null) return true;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
  for (let i=0;i<inputs.length;i++) {
    if (!valid()) return false;
    setter.call(inputs[i],values[fields[i].id]);
    inputs[i].dispatchEvent(new Event('input',{bubbles:true}));
    inputs[i].dispatchEvent(new Event('change',{bubbles:true}));
  }
  return valid() && inputs.every((el,i)=>el.value===values[fields[i].id]);
}`;
export async function secureBrowserForm(options:{cdp:PrivateBrowserChannel; request:BrowserVaultIdentity; fields:SecureFormField[]; signal?:AbortSignal; expectedLoaderId?:string; values?:Record<string,string>; quarantine?: (loaderId:string)=>Promise<void>}):Promise<{loaderId:string;status:'filled'|'ready'|'outcome_unknown'}> {
  try {
    options.signal?.throwIfAborted();
    const world = await privateWorld(options.cdp, options.request);
    if (!world || (options.expectedLoaderId !== undefined && world.loaderId !== options.expectedLoaderId)) throw new Error();
    if (options.values) { if (!options.quarantine) throw new Error(); await options.quarantine(world.loaderId); }
    options.signal?.throwIfAborted();
    let result;
    try { result = await options.cdp.send('Runtime.callFunctionOn', {executionContextId:world.executionContextId,functionDeclaration:SECURE_FORM_FILL_FUNCTION,
      arguments:[options.request.expected_origin,options.fields,options.values ?? null].map(value=>({value})),returnByValue:true,silent:true},world.sessionId); }
    catch { if (options.values) return {loaderId:world.loaderId,status:'outcome_unknown'}; throw new Error(); }
    if (result?.exceptionDetails || result?.result?.value !== true) { if (options.values) return {loaderId:world.loaderId,status:'outcome_unknown'}; throw new Error(); }
    return {loaderId:world.loaderId,status:options.values ? 'filled':'ready'};
  } catch { throw new Error('Supported secure form is unavailable'); }
}
