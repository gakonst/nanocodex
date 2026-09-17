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
  readonly socket: WebSocket;
  constructor(socket: WebSocket) {
    this.socket = socket;
    socket.accept();
    socket.addEventListener("message", event => {
      try {
        if (typeof event.data !== "string") return;
        const message = JSON.parse(event.data);
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
  #reject() {
    this.#closed = true;
    for (const entry of this.#pending.values()) { clearTimeout(entry.timer); entry.reject(new Error("Private browser disconnected")); }
    this.#pending.clear();
  }
  close() { this.#reject(); try { this.socket.close(1000, "Finished"); } catch { /* No provider errors escape. */ } }
}

/** A fixed function, executed in a fresh isolated world. Selectors are data, never code.
 * Restrict to a visible, same-origin POST login form in the top frame. Atomic checks
 * and native setters prevent page script from swapping the destination between awaits.
 */
export const BROWSER_VAULT_FILL_FUNCTION = `function(origin, usernameSelector, passwordSelector, username, password, submit) {
  if (window !== window.top || location.origin !== origin || location.protocol !== "https:") return false;
  const one = selector => { const nodes = document.querySelectorAll(selector); return nodes.length === 1 ? nodes[0] : null; };
  const user = usernameSelector === null ? null : one(usernameSelector);
  const pass = passwordSelector === null ? null : one(passwordSelector);
  const visible = input => {
    if (!(input instanceof HTMLInputElement) || !input.isConnected || input.disabled || input.readOnly
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
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  if (user) setter.call(user, username);
  if (pass) setter.call(pass, password);
  // Native submission bypasses page callbacks and submits only to the checked
  // same-origin POST action. SPA/custom login handlers require human takeover.
  if (submit) HTMLFormElement.prototype.submit.call(form);
  return true;
}`;

export async function fillBrowserVault(options: {
  cdp: Pick<PrivateBrowserCdp, "send">;
  sessionId: string;
  request: BrowserVaultRequest;
  resolve: () => Promise<BrowserVaultLogin>;
  quarantine: (value: BrowserVaultQuarantine) => Promise<void>;
  signal?: AbortSignal;
}): Promise<{ status: "submitted" | "filled" }> {
  try {
    const { cdp, request } = options;
    const checkAbort = () => { if (options.signal?.aborted) throw new Error(); };
    checkAbort();
    const target = await cdp.send("Target.getTargetInfo", { targetId: request.target_id });
    if (target?.targetInfo?.type !== "page" || new URL(target.targetInfo.url).origin !== request.expected_origin) throw new Error();
    const attached = await cdp.send("Target.attachToTarget", { targetId: request.target_id, flatten: true });
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
    if (result?.exceptionDetails || result?.result?.value !== true) throw new Error();
    return { status: request.submit ? "submitted" : "filled" };
  } catch { throw new Error("Vault login could not be filled safely"); }
}

/** Fixed, metadata-only inspection. No page text, URLs, attributes or values escape. */
export async function inspectBrowserVault(cdp: Pick<PrivateBrowserCdp, "send">, request: BrowserVaultRequest): Promise<{
  status: "login_form" | "username_form" | "password_form" | "no_supported_login_form" | "destination_changed";
  username_selector?: string; password_selector?: string;
}> {
  try {
    const target = await cdp.send("Target.getTargetInfo", { targetId: request.target_id });
    if (target?.targetInfo?.type !== "page") throw new Error();
    if (new URL(target.targetInfo.url).origin !== request.expected_origin) return { status: "destination_changed" };
    const attached = await cdp.send("Target.attachToTarget", { targetId: request.target_id, flatten: true });
    const tree = await cdp.send("Page.getFrameTree", {}, attached.sessionId);
    const frame = tree?.frameTree?.frame;
    if (!frame || frame.parentId || new URL(frame.url).origin !== request.expected_origin) throw new Error();
    const world = await cdp.send("Page.createIsolatedWorld", { frameId: frame.id, worldName: "nanocodex-vault-status" }, attached.sessionId);
    const username = 'input:not([type]),input[type="text"],input[type="email"]';
    const password = 'input[type="password"]';
    const result = await cdp.send("Runtime.callFunctionOn", {
      executionContextId: world.executionContextId,
      functionDeclaration: `function(origin, username, password) {
        if (window !== window.top || location.origin !== origin) return null;
        const usable = selector => {
          const nodes = document.querySelectorAll(selector);
          if (nodes.length !== 1) return false;
          const input = nodes[0], form = input.form;
          return !input.disabled && !input.readOnly && input.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})
            && !!form && form.method.toLowerCase() === 'post' && new URL(form.action).origin === origin;
        };
        return [usable(username), usable(password)];
      }`,
      arguments: [request.expected_origin, username, password].map(value => ({ value })), returnByValue: true, silent: true,
    }, attached.sessionId);
    const flags = result?.result?.value;
    if (result?.exceptionDetails || !Array.isArray(flags) || flags.length !== 2 || flags.some(v => typeof v !== "boolean")) throw new Error();
    return { status: flags[0] && flags[1] ? "login_form" : flags[0] ? "username_form" : flags[1] ? "password_form" : "no_supported_login_form",
      ...(flags[0] ? { username_selector: username } : {}), ...(flags[1] ? { password_selector: password } : {}) };
  } catch { throw new Error("Private login status is unavailable"); }
}
