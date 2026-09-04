import { Mppx, tempo as mppTempo } from "mppx/client";
import { Client, Dialog, Transport, type Connection } from "nanocodex/connect";
import { createWalletClient, custom } from "viem";
import { tempo as tempoChain } from "viem/chains";
import { MACH, TEMPO_CHAIN_ID, TIP20_CHANNEL_ESCROW, USDC_E } from "./policy";

type AppConfiguration = Readonly<{
  amount: "0" | "0.1";
  chain_id: number;
  connect: Readonly<{
    api_url: string;
    app_id: string;
    app_origin: string;
    dialog_url: string;
  }>;
  currency: `0x${string}`;
  payment_enabled: boolean;
  recipient?: `0x${string}`;
}>;

type TrialView = Readonly<{
  error?: string;
  final_message?: string;
  payment_reference?: string;
  phase: "available" | "payment_pending" | "paid" | "running" | "completed" | "failed";
}>;

type StoredGrant = Readonly<{ grantId: `0x${string}`; token: string }>;

const CONNECT_RESOURCES = [
  "urn:nanocodex:agent:run",
  "urn:nanocodex:capability:mercator:boost",
  "urn:nanocodex:mpp:machusd:spend",
] as const;
const MERCATOR_SETTLEMENT = "0xa295c42fbcc026a62304a7701f25b4c91799b0da" as const;
const DEFAULT_ACCESS_KEY_LIMIT = 10_000_000n;
const ASTRA_ACCESS_KEY_LIMIT = 100_000n;
const ACCESS_KEY_PERIOD = 86_400;

const elements = {
  account: requiredElement("account"),
  connect: requiredButton("connect"),
  error: requiredElement("error"),
  fund: requiredButton("fund"),
  form: requiredElement("prompt-form") as HTMLFormElement,
  intro: requiredElement("intro"),
  logout: requiredButton("logout"),
  payment: requiredElement("payment"),
  prompt: requiredElement("prompt") as HTMLTextAreaElement,
  result: requiredElement("result"),
  status: requiredElement("status"),
  submit: requiredButton("submit"),
};

let configuration: AppConfiguration;
let connection: Connection | undefined;
let grant: StoredGrant | undefined;
let client: ReturnType<typeof createAppClient>;
let busy = false;
let trialClaimed = false;

void initialize().catch(showError);

async function initialize(): Promise<void> {
  configuration = await apiJson<AppConfiguration>("/api/config");
  if (configuration.chain_id !== TEMPO_CHAIN_ID || configuration.currency.toLowerCase() !== MACH) {
    throw new Error("The Astra trial payment policy is invalid.");
  }
  const storage = recordingStorage();
  client = createAppClient(configuration, storage);

  elements.payment.textContent = configuration.amount === "0"
    ? "Local proof mode: your wallet signs, but no MACH moves."
    : "$0.10 in MACH · one prompt · gas sponsored";
  elements.connect.addEventListener("click", () => void connectAccount(storage));
  elements.logout.addEventListener("click", () => void disconnectAccount());
  elements.fund.addEventListener("click", () => void fundAccount());
  elements.form.addEventListener("submit", (event) => void submitPrompt(event));

  const restored = await client.connection.reconnect(connectRequest()).catch(() => undefined);
  if (restored) {
    connection = restored;
    grant = storage.grant();
    if (grant) await establishSession();
  }
  render();
}

function createAppClient(
  app: AppConfiguration,
  storage: ReturnType<typeof recordingStorage>,
) {
  const apiUrl = app.connect.api_url.replace(/\/+$/, "");
  const accessKey = accessKeyPolicy(app);
  return Client.create({
    appId: app.connect.app_id,
    appOrigin: app.connect.app_origin,
    auth: {
      challenge: `${apiUrl}/v1/connect/auth/challenge`,
      verify: `${apiUrl}/v1/connect/auth`,
      logout: `${apiUrl}/v1/connect/auth/logout`,
      resources: CONNECT_RESOURCES,
      returnToken: true,
    },
    accessKey: {
      authorize: {
        expiry: Math.floor(Date.now() / 1_000) + 30 * 86_400,
        limits: accessKey.limits,
        reuse: {
          minExpiry: Math.floor(Date.now() / 1_000) + 7 * 86_400,
          minLimits: accessKey.limits,
        },
        scopes: accessKey.scopes,
      },
    },
    dialog: Dialog.popup({
      host: app.connect.dialog_url,
      key: "astra-one-shot",
      name: "Astra One-Shot",
    }),
    session: storage,
    transport: Transport.http(app.connect.api_url),
  });
}

function accessKeyPolicy(app: AppConfiguration) {
  if (app.amount === "0.1") {
    if (!app.payment_enabled || !app.recipient) {
      throw new Error("The Astra trial payment recipient is not configured.");
    }
    return {
      limits: [{ token: MACH, limit: ASTRA_ACCESS_KEY_LIMIT, period: ACCESS_KEY_PERIOD }],
      scopes: [{ address: MACH, selector: "0x95777d59", recipients: [app.recipient] }],
    } as const;
  }
  return {
    limits: [
      { token: MACH, limit: DEFAULT_ACCESS_KEY_LIMIT, period: ACCESS_KEY_PERIOD },
      { token: USDC_E, limit: DEFAULT_ACCESS_KEY_LIMIT, period: ACCESS_KEY_PERIOD },
    ],
    scopes: [
      { address: USDC_E, selector: "0xa9059cbb", recipients: [MERCATOR_SETTLEMENT] },
      { address: USDC_E, selector: "0x95777d59", recipients: [MERCATOR_SETTLEMENT] },
      { address: MACH, selector: "0xa9059cbb", recipients: [MERCATOR_SETTLEMENT] },
      { address: MACH, selector: "0x95777d59", recipients: [MERCATOR_SETTLEMENT] },
      { address: TIP20_CHANNEL_ESCROW, selector: "0xedc53b00" },
      { address: TIP20_CHANNEL_ESCROW, selector: "0xdc48471e" },
    ],
  } as const;
}

function connectRequest() {
  return {
    authorization: "access_key" as const,
    capabilities: {
      agent: {
        finalMessages: false,
        actionSummaries: false,
        conversationHistory: false,
        rawTraces: false,
      },
    },
    permission: "agent.run",
  };
}

async function connectAccount(storage: ReturnType<typeof recordingStorage>): Promise<void> {
  if (busy) return;
  setBusy(true, "Opening Nanocodex Connect…");
  try {
    connection = await client.connection.connect(connectRequest());
    grant = storage.grant();
    if (!grant || grant.grantId.toLowerCase() !== connection.grant.id.toLowerCase()) {
      throw new Error("Nanocodex Connect did not retain an app session.");
    }
    await establishSession();
  } catch (error) {
    connection = undefined;
    grant = undefined;
    trialClaimed = false;
    showError(error);
  } finally {
    setBusy(false);
    render();
  }
}

async function establishSession(): Promise<void> {
  const session = await authenticatedJson<{ account_address: string; authenticated: true }>("/api/session");
  if (!connection || session.account_address.toLowerCase() !== connection.accountAddress.toLowerCase()) {
    throw new Error("The Connect account does not match the app session.");
  }
  elements.account.textContent = shortAddress(connection.accountAddress);
  await refreshTrial();
}

async function disconnectAccount(): Promise<void> {
  if (busy) return;
  setBusy(true, "Disconnecting…");
  try {
    await client.connection.disconnect();
  } catch {
    // The local app session is cleared even when the remote grant has already expired.
  } finally {
    connection = undefined;
    grant = undefined;
    trialClaimed = false;
    elements.account.textContent = "Not connected";
    elements.result.textContent = "";
    elements.status.textContent = "Connect to claim your one prompt.";
    setBusy(false);
    render();
  }
}

async function fundAccount(): Promise<void> {
  if (!connection || busy) return;
  setBusy(true, "Opening the $5 MACH onramp…");
  try {
    await client.machineUsd.fund({
      accountAddress: connection.accountAddress,
      grantId: connection.grant.id,
      usdAmountCents: 500,
    });
    elements.status.textContent = "MACH funded. Your one Astra prompt is ready.";
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
    render();
  }
}

async function submitPrompt(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (!connection || !grant || busy) return;
  const prompt = elements.prompt.value.trim();
  if (!prompt) return;
  if (!configuration.payment_enabled || !configuration.recipient) {
    showError(new Error("The trial payment or sponsor account is not configured."));
    return;
  }
  setBusy(true, configuration.amount === "0" ? "Requesting wallet proof…" : "Authorizing 0.1 MACH…");
  const requestKey = crypto.randomUUID();
  try {
    let response: Response;
    try {
      // Popup Connect intentionally closes after login. Reopen it for the
      // delegated access-key signature, then close it as soon as MPP settles.
      client.dialog.showWallet?.();
      const wallet = createWalletClient({
        account: connection.accountAddress,
        chain: tempoChain,
        transport: custom(client.provider),
      });
      const payments = Mppx.create({
        methods: [mppTempo.charge({
          account: connection.accountAddress,
          expectedChainId: TEMPO_CHAIN_ID,
          expectedRecipients: [configuration.recipient],
          getClient: () => wallet,
          mode: "pull",
        })],
        polyfill: false,
      });
      response = await payments.fetch("/api/prompt", {
        method: "POST",
        headers: authenticatedHeaders({
          "content-type": "application/json",
          "idempotency-key": requestKey,
        }),
        body: JSON.stringify({ prompt }),
      });
    } finally {
      client.dialog.hideWallet?.();
    }
    const view = await response.json() as TrialView & { error?: string };
    if (!response.ok) throw new Error(humanError(view.error));
    presentTrial(view);
    if (view.phase === "running" || view.phase === "paid") await pollTrial();
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
    render();
  }
}

async function refreshTrial(): Promise<void> {
  const response = await authenticatedFetch("/api/trial");
  const view = await response.json() as TrialView & { error?: string };
  if (!response.ok) throw new Error(humanError(view.error));
  presentTrial(view);
  if (view.phase === "running" || view.phase === "paid") void pollTrial();
}

async function pollTrial(): Promise<void> {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 1_000));
    const response = await authenticatedFetch("/api/trial");
    const view = await response.json() as TrialView;
    if (!response.ok) throw new Error("Unable to refresh the Astra prompt.");
    presentTrial(view);
    if (view.phase === "completed" || view.phase === "failed" || view.phase === "payment_pending") return;
  }
  throw new Error("The Astra prompt is still running. Reload to check it again.");
}

function presentTrial(view: TrialView): void {
  elements.result.textContent = view.final_message ?? "";
  elements.status.textContent = ({
    available: "One Astra prompt is available for this Nanocodex account.",
    payment_pending: "Payment outcome is pending. No second charge will be attempted.",
    paid: "Payment accepted. Creating the locked Astra agent…",
    running: "Astra is thinking at max effort…",
    completed: view.payment_reference
      ? `Completed · payment ${shortReference(view.payment_reference)}`
      : "Completed.",
    failed: view.error ?? "The Astra prompt failed.",
  } satisfies Record<TrialView["phase"], string>)[view.phase];
  const claimed = view.phase !== "available";
  trialClaimed = claimed;
  elements.prompt.disabled = claimed || busy;
  elements.submit.disabled = claimed || busy;
}

function authenticatedFetch(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, { ...init, headers: authenticatedHeaders(init.headers) });
}

async function authenticatedJson<value>(path: string): Promise<value> {
  const response = await authenticatedFetch(path);
  const body = await response.json() as value & { error?: string };
  if (!response.ok) throw new Error(humanError(body.error));
  return body;
}

function authenticatedHeaders(headers?: HeadersInit): Headers {
  if (!grant) throw new Error("Connect to Nanocodex first.");
  const result = new Headers(headers);
  result.set("authorization", `Bearer ${grant.token}`);
  result.set("x-nanocodex-grant-id", grant.grantId);
  return result;
}

async function apiJson<value>(path: string): Promise<value> {
  const response = await fetch(path, { headers: { accept: "application/json" } });
  const body = await response.json() as value & { error?: string };
  if (!response.ok) throw new Error(humanError(body.error));
  return body;
}

function recordingStorage() {
  const prefix = "astra-trial:";
  let latest: string | null = null;
  return {
    getItem(key: string) {
      latest = sessionStorage.getItem(`${prefix}${key}`);
      return latest;
    },
    setItem(key: string, value: string) {
      latest = value;
      sessionStorage.setItem(`${prefix}${key}`, value);
    },
    removeItem(key: string) {
      latest = null;
      sessionStorage.removeItem(`${prefix}${key}`);
    },
    grant(): StoredGrant | undefined {
      if (!latest) return undefined;
      try {
        const value = JSON.parse(latest) as Record<string, unknown>;
        return typeof value.grantId === "string" && /^0x[0-9a-fA-F]{64}$/.test(value.grantId)
          && typeof value.token === "string" && /^[A-Za-z0-9_-]{43}$/.test(value.token)
          ? { grantId: value.grantId.toLowerCase() as `0x${string}`, token: value.token }
          : undefined;
      } catch {
        return undefined;
      }
    },
  };
}

function render(): void {
  const connected = Boolean(connection && grant);
  elements.intro.hidden = connected;
  elements.form.hidden = !connected;
  elements.logout.hidden = !connected;
  elements.fund.hidden = !connected || configuration.amount === "0";
  elements.connect.disabled = busy;
  elements.logout.disabled = busy;
  elements.fund.disabled = busy;
  elements.prompt.disabled = busy || trialClaimed;
  elements.submit.disabled = busy || trialClaimed;
}

function setBusy(value: boolean, message?: string): void {
  busy = value;
  elements.error.textContent = "";
  if (message) elements.status.textContent = message;
  render();
}

function showError(error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  elements.error.textContent = message;
}

function humanError(value: string | undefined): string {
  return ({
    astra_agent_unavailable: "The sponsor Astra account is temporarily unavailable.",
    astra_trial_already_claimed: "This Nanocodex account has already claimed its prompt.",
    connect_session_invalid: "Your Nanocodex Connect session expired. Connect again.",
    connect_session_required: "Connect to Nanocodex first.",
    payment_account_mismatch: "The paying wallet must match the connected Nanocodex account.",
    payment_outcome_pending: "The payment outcome is pending and will not be charged twice.",
  } as Record<string, string>)[value ?? ""] ?? "The request could not be completed.";
}

function shortAddress(value: string): string {
  return `${value.slice(0, 6)}…${value.slice(-4)}`;
}

function shortReference(value: string): string {
  return value.length > 18 ? `${value.slice(0, 10)}…${value.slice(-6)}` : value;
}

function requiredElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing #${id}`);
  return element;
}

function requiredButton(id: string): HTMLButtonElement {
  return requiredElement(id) as HTMLButtonElement;
}
