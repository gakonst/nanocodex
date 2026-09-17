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
  sessionId: string; targetId: string; loaderId: string; origin: string; vaultId: string;
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

/** No SDK/debug ring or model dispatcher ever receives privileged CDP traffic. */
export class PrivateBrowserCdp {
  #id = 0;
  #closed = false;
  #pending = new Map<number, { resolve(value: any): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }>();
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
        if (message.error) pending.reject(new Error("Private browser operation failed"));
        else pending.resolve(message.result);
      } catch { this.close(); }
    });
    socket.addEventListener("close", () => this.#reject());
    socket.addEventListener("error", () => this.#reject());
  }
  static async connect(browser: BrowserBinding, sessionId: string, signal?: AbortSignal): Promise<PrivateBrowserCdp> {
    const response = await browser.fetch(`https://localhost/v1/devtools/browser/${encodeURIComponent(sessionId)}`, {
      headers: { Upgrade: "websocket" },
      signal: AbortSignal.any([AbortSignal.timeout(10_000), ...(signal ? [signal] : [])]),
    });
    if (!response.webSocket) throw new Error("Private browser unavailable");
    return new PrivateBrowserCdp(response.webSocket);
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
      this.#pending.set(id, { resolve, reject, timer });
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
  const safeLoginForm = form => {
    if (!(form instanceof HTMLFormElement) || !form.isConnected || form.getRootNode() !== document
      || location.origin !== origin || form.method.toLowerCase() !== 'post' || (form.target && form.target !== '_self')) return false;
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
 * Restrict to a visible, same-origin POST login form in the top frame. Atomic checks
 * and native setters are followed by input/change events and destination rechecks.
 */
export const BROWSER_VAULT_FILL_FUNCTION = `function(origin, usernameSelector, passwordSelector, username, password, submit) {
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
  if (!form) return false;
  const action = new URL(form.action, location.href);
  if (action.origin !== origin || action.username || action.password || form.method.toLowerCase() !== 'post'
    || (form.target && form.target !== '_self')) return false;
  const challenge = () => [...document.querySelectorAll('iframe,[id],[class]')].slice(0,5000).some(el =>
    el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) && /captcha|turnstile|challenge-platform/i.test([el.id, typeof el.className === 'string' ? el.className : '', el instanceof HTMLIFrameElement ? el.src : ''].join(' ')));
  const valid = () => !challenge() && safeLoginForm(form)
    && (!user || (one(usernameSelector) === user && user.form === form && visible(user) && ['text','email'].includes(user.type)))
    && (!pass || (one(passwordSelector) === pass && pass.form === form && visible(pass) && pass.type === 'password'));
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
  if (submit) {
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
  resolve: () => Promise<BrowserVaultLogin>;
  quarantine: (value: BrowserVaultQuarantine) => Promise<void>;
  signal?: AbortSignal;
}): Promise<{ status: "submitted" | "filled"; submission?: "action_required" }> {
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
      || new URL(frame.url).origin !== request.expected_origin) throw new Error();
    const world = await cdp.send("Page.createIsolatedWorld", { frameId: frame.id, worldName: "nanocodex-vault", grantUniveralAccess: false }, sid);
    if (!Number.isInteger(world?.executionContextId)) throw new Error();
    const login = await options.resolve();
    checkAbort();
    if ((request.username_selector && !login.username) || (request.password_selector && !login.password)) throw new Error();
    // Persist before any secret reaches the browser, including ambiguous failures.
    await options.quarantine({ sessionId: options.sessionId, targetId: request.target_id, loaderId: frame.loaderId, origin: request.expected_origin, vaultId: request.vault_id });
    checkAbort();
    const result = await cdp.send("Runtime.callFunctionOn", {
      executionContextId: world.executionContextId,
      functionDeclaration: BROWSER_VAULT_FILL_FUNCTION,
      arguments: [request.expected_origin, request.username_selector ?? null, request.password_selector ?? null, request.username_selector ? login.username : null, request.password_selector ? login.password : null, request.submit].map(value => ({ value })),
      returnByValue: true,
      silent: true,
    }, sid);
    if (!result?.exceptionDetails && result?.result?.value === "unsupported") return { status: "filled", submission: "action_required" };
    if (result?.exceptionDetails || result?.result?.value !== true) throw new Error();
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
  elements: { ref: string; role: "link" | "button"; text: string }[];
};
const USERNAME_SELECTOR = 'input:not([type]):not([autocomplete="one-time-code"]),input[type="text"]:not([autocomplete="one-time-code"]),input[type="email"]';
const PASSWORD_SELECTOR = 'input[type="password"]';
const OTP_SELECTOR = 'input[autocomplete="one-time-code"],input[name="otp"],input[name="code"],input[name="verification_code"]';

/** Fixed code in an isolated world: no caller JavaScript, DOM values, raw attributes,
 * scripts, subframes or hidden content are returned. Refs bind to node identity and
 * a single snapshot. Actions deliberately support native same-origin links and
 * POST form submission and explicit bounded login controls. Other custom controls
 * require human takeover; authorized page handlers can perform their own requests.
 */
export const BROWSER_VAULT_CONTINUATION_FUNCTION = `function(origin, mode, snapshotId, ref, url, selectors) {
  if (window !== window.top || location.origin !== origin || location.protocol !== 'https:') return null;
  ${VAULT_FORM_SUBMISSION}
  const visible = (el, readingText = false) => el instanceof Element && el.isConnected && el.getRootNode() === document
    && !el.closest('[inert],[hidden],script,style,noscript,template,textarea,select' + (readingText ? '' : ',[aria-hidden="true"]'))
    && el.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) && el.getClientRects().length > 0;
  const safeUrl = value => { try { const u = new URL(value, location.href); return u.origin === origin && !u.username && !u.password ? u.href : null; } catch { return null; } };
  const safeForm = form => form instanceof HTMLFormElement && form.method.toLowerCase() === 'post'
    && (!form.target || form.target === '_self') && safeUrl(form.action);
  const associatedForm = el => el.form || el.closest('form');
  const customLogin = el => el instanceof HTMLElement && el.getAttribute('role') === 'button'
    && !el.matches('a,input,button') && !el.closest('a,button,label,summary') && el.getAttribute('aria-disabled') !== 'true'
    && /^(log in|login|sign in)$/i.test((el.textContent || '').trim())
    && safeForm(associatedForm(el)) && [...associatedForm(el).elements].some(input =>
      input instanceof HTMLInputElement && ['text','email','password'].includes(input.type) && visible(input) && !input.disabled && !input.readOnly);
  const usable = selector => {
    const nodes = document.querySelectorAll(selector);
    return nodes.length === 1 && nodes[0] instanceof HTMLInputElement && visible(nodes[0])
      && !nodes[0].disabled && !nodes[0].readOnly && !!safeForm(nodes[0].form);
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
  if (mode === 'click') {
    const snapshot = globalThis.__nanocodexVaultSnapshot;
    if (!snapshot) return 'snapshot_missing';
    if (snapshot.id !== snapshotId) return 'stale_ref';
    if (snapshot.href !== location.href) return 'document_changed';
    const entry = snapshot.nodes.get(ref), el = entry && entry.el;
    delete globalThis.__nanocodexVaultSnapshot;
    if (challenge) return 'challenge_detected';
    if (!el) return 'stale_ref';
    if (!visible(el) || el.disabled || el.getAttribute('aria-disabled') === 'true') return 'element_not_visible';
    if (el.outerHTML !== entry.html
      || (entry.form && (associatedForm(el) !== entry.form || entry.form.outerHTML !== entry.formHtml))) return 'changed_element';
    el.scrollIntoView({block:"center", inline:"center", behavior:"instant"});
    const rect = el.getBoundingClientRect();
    if (rect.left < 0 || rect.top < 0 || rect.right > innerWidth || rect.bottom > innerHeight) return 'outside_viewport';
    if (!el.contains(document.elementFromPoint(rect.left + rect.width/2, rect.top + rect.height/2))) return 'occluded';
    if (el instanceof HTMLAnchorElement && (!el.target || el.target === '_self') && !el.hasAttribute('download')) {
      const destination = safeUrl(el.href);
      if (!destination) return 'unsafe_destination';
      location.assign(destination); return true;
    }
    if (customLogin(el)) { HTMLElement.prototype.click.call(el); return true; }
    if ((el instanceof HTMLButtonElement || el instanceof HTMLInputElement) && el.type === 'submit' && !el.name && safeForm(el.form)
      && !['formaction','formmethod','formtarget','formenctype','formnovalidate'].some(a => el.hasAttribute(a))) {
      return submitLoginForm(el.form, el);
    }
    return 'unsupported_element';
  }
  if (mode !== 'snapshot') return null;
  const readable = root => {
    const chunks = [], walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node, visited = 0, length = 0;
    while ((node = walker.nextNode()) && ++visited <= 10000) {
      const parent = node.parentElement;
      if (!parent || !visible(parent, true) || parent.closest('input,option')) continue;
      const value = node.textContent || '';
      if (!value.trim() || value.length > 8192) continue; // Omit whole nodes; never return a truncated secret.
      if (length + value.length > 32768) break;
      chunks.push(value); length += value.length;
    }
    return chunks.join(' ');
  };
  const nodes = new Map(), elements = [];
  for (const el of [...document.querySelectorAll('a[href],button,input[type="submit"],[role="button"]')].slice(0, 2000)) {
    if (elements.length >= 200) break;
    if (!visible(el) || el.disabled || el.getAttribute('aria-disabled') === 'true') continue;
    const link = el instanceof HTMLAnchorElement;
    if (link ? (!safeUrl(el.href) || (el.target && el.target !== '_self') || el.hasAttribute('download'))
      : (!customLogin(el) && !safeSubmitter(el))) continue;
    if (!link && !safeForm(associatedForm(el))) continue;
    const key = 'e' + (elements.length + 1);
    nodes.set(key, {el, html:el.outerHTML, form:link ? null : associatedForm(el), formHtml:link ? null : associatedForm(el).outerHTML});
    // Input values, including submit values, are never read.
    elements.push({ref:key, role:link ? 'link' : 'button', text:el instanceof HTMLInputElement ? '' : readable(el)});
  }
  globalThis.__nanocodexVaultSnapshot = {id:snapshotId, href:location.href, nodes};
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
async function continuation(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity, mode: string, snapshotId = "", ref = "", url = "") {
  const world = await privateWorld(cdp, request);
  if (!world) return null;
  const result = await cdp.send("Runtime.callFunctionOn", {
    executionContextId: world.executionContextId, functionDeclaration: BROWSER_VAULT_CONTINUATION_FUNCTION,
    arguments: [request.expected_origin, mode, snapshotId, ref, url, [USERNAME_SELECTOR, PASSWORD_SELECTOR, OTP_SELECTOR]].map(value => ({ value })),
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
      if (!el || !/^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/.test(el.ref) || !["link", "button"].includes(el.role) || typeof el.text !== "string" || el.text.length > 65536) throw new Error();
      return { ref: el.ref as string, role: el.role as "link" | "button", text: clean(el.text, 256) };
    });
    return { ...status, snapshot_id: id, title: clean(value.title, 256), text: clean(value.text, 12000), elements };
  } catch { throw new Error("Private page snapshot is unavailable"); }
}

// Only these fixed host-owned categories may cross the private diagnostic boundary.
const CLICK_FAILURES = ["snapshot_missing", "stale_ref", "document_changed", "challenge_detected",
  "element_not_visible", "changed_element", "outside_viewport", "occluded", "unsafe_destination", "unsupported_element"] as const;
type BrowserVaultClickFailure = typeof CLICK_FAILURES[number];
export class BrowserVaultActionRejected extends Error {
  constructor(reason: BrowserVaultClickFailure) {
    super(`Private browser action could not be completed safely (${reason})`);
    this.name = "BrowserVaultActionRejected";
  }
}

export type BrowserVaultAction = { action: "click"; snapshot_id: string; ref: string } | { action: "navigate"; url: string };
export async function actBrowserVault(cdp: PrivateBrowserChannel, request: BrowserVaultIdentity, action: BrowserVaultAction): Promise<{ status: "navigation_requested" | "action_requested" }> {
  try {
    if (action.action === "navigate") {
      const destination = new URL(action.url);
      if (action.url.length > 8192 || destination.origin !== request.expected_origin || destination.username || destination.password) throw new Error();
      if (await continuation(cdp, request, "navigate", "", "", destination.href) !== true) throw new Error();
      return { status: "navigation_requested" };
    }
    if (action.action !== "click" || !/^[0-9a-f-]{36}$/.test(action.snapshot_id) || !/^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/.test(action.ref)) throw new Error();
    const result = await continuation(cdp, request, "click", action.snapshot_id, action.ref);
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
