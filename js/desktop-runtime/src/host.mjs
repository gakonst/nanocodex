import { createInterface } from "node:readline";
import { DesktopRuntime, DEFAULT_ORIGIN, managedOrigin } from "./runtime.mjs";
import { desktopDefaults, desktopEnvironment, desktopPreferences } from "./configuration.mjs";
import { SmsSignIn } from "./auth.mjs";

// stdout is exclusively the versioned-by-package JSONL protocol. Incidental
// dependency diagnostics must never corrupt a native client's message stream.
console.log = (...values) => console.error(...values);
const safeError = error => String(error?.message ?? error).replace(/ncx_live_[A-Za-z0-9_-]+/g, "[redacted]").slice(0, 500);
const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const methods = new Set([
  "state", "connect", "disconnect", "refresh", "openThread", "closeThread", "older",
  "createThread", "prompt", "steer", "cancel", "settings", "saveLayout", "saveHand",
  "startHand", "stopHand", "removeHand", "prepareFolderHand",
  "startSignIn", "verifySignIn", "completeSignIn", "cancelSignIn",
]);
const environment = await desktopEnvironment();
const apiKey = environment.NANOCODEX_API_KEY || environment.NC_API_KEY;
const baseUrl = environment.NANOCODEX_MANAGED_URL || DEFAULT_ORIGIN;
const preferences = await desktopPreferences({ directory: environment.NANOCODEX_DESKTOP_DATA, apiKey, baseUrl });
const runtime = new DesktopRuntime({ apiKey, baseUrl, dataDirectory: environment.NANOCODEX_DESKTOP_DATA, defaults: await desktopDefaults(environment), ...preferences });
let signIn;
let signInOrigin;
let signInCredential;
let activeCredential = apiKey ? { baseUrl, apiKey } : undefined;
let authQueue = Promise.resolve();
const signInIsActive = () => signInCredential && activeCredential
  && signInCredential.apiKey === activeCredential.apiKey && signInCredential.baseUrl === activeCredential.baseUrl;
const discardSignIn = async () => {
  // Swift may have persisted and connected the key just before its completion
  // acknowledgement was lost. Never revoke the credential still in use.
  if (signInIsActive()) await signIn?.complete();
  else await signIn?.cancel();
  signIn = undefined;
  signInCredential = undefined;
};
const authActions = {
  async startSignIn({ phone, baseUrl: origin } = {}) {
    const nextOrigin = managedOrigin(origin || runtime.state().baseUrl);
    if (signIn && (signInOrigin !== nextOrigin || signInIsActive())) await discardSignIn();
    if (!signIn) { signIn = new SmsSignIn({ baseUrl: nextOrigin }); signInOrigin = nextOrigin; }
    return signIn.start({ phone });
  },
  async verifySignIn(input) {
    if (!signIn) throw new Error("Request a text message code first.");
    signInCredential = await signIn.verify(input);
    return signInCredential;
  },
  async completeSignIn() { await signIn?.complete(); signIn = undefined; signInCredential = undefined; },
  cancelSignIn: discardSignIn,
  async connect(input) {
    const state = await runtime.connect(input);
    activeCredential = { baseUrl: state.baseUrl, apiKey: input.apiKey };
    return state;
  },
  async disconnect() {
    const state = await runtime.disconnect();
    activeCredential = undefined;
    return state;
  },
};
let closing = false;
const inFlight = new Set();
runtime.on("event", event => { if (!closing) send({ event }); });

const input = createInterface({ input: process.stdin, crlfDelay: Infinity, terminal: false });
input.on("line", line => {
  if (closing) return;
  let request;
  try {
    if (Buffer.byteLength(line) > 2_000_000) throw new Error("Request is too large.");
    request = JSON.parse(line);
    if (!request || !["string", "number"].includes(typeof request.id) || !methods.has(request.method)
      || !Array.isArray(request.args) || request.args.length > 1) throw new Error("Invalid desktop request.");
    if (inFlight.has(request.id)) throw new Error("Duplicate request ID.");
  } catch (error) { send({ id: request?.id ?? null, error: safeError(error) }); return; }
  inFlight.add(request.id);
  Promise.resolve().then(() => {
    if (!Object.hasOwn(authActions, request.method)) return runtime[request.method](...request.args);
    const pending = authQueue.then(() => authActions[request.method](...request.args));
    authQueue = pending.catch(() => {});
    return pending;
  })
    .then(result => { if (!closing) send({ id: request.id, result: result ?? null }); })
    .catch(error => { if (!closing) send({ id: request.id, error: safeError(error) }); })
    .finally(() => inFlight.delete(request.id));
});
const close = async () => {
  if (closing) return;
  closing = true;
  input.close();
  try {
    try { await authQueue; await discardSignIn(); }
    finally { try { await runtime.close(); } finally { await preferences.close(); } }
  }
  catch (error) { process.stderr.write(`${safeError(error)}\n`); process.exitCode = 1; }
  // A native host is scoped to its app process. End retained HTTP pool handles
  // once every Hand and preference write has finished shutting down.
  process.exit(process.exitCode ?? 0);
};
input.on("close", () => { void close(); });
process.on("SIGTERM", () => { void close(); });
process.on("SIGINT", () => { void close(); });
process.stdout.on("error", () => { void close(); });
send({ event: { type: "state", state: runtime.state() } });
void runtime.refresh();
