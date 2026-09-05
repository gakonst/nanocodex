const ADDRESS = /^0x[0-9a-f]{40}$/i;
const TRANSACTION_HASH = /^0x[0-9a-f]{64}$/i;
const INTEGER = /^(?:0|[1-9][0-9]*)$/;
const MACHINE_USD = "0x20c000000000000000000000f37de3740adec032";
const TEMPO_CHAIN_ID = 4217;

export type WalletBalance = Readonly<{
  account: `0x${string}`;
  atomics: bigint;
  decimals: 6;
  symbol: "MACH";
}>;

export type MachineUsdConfig = Readonly<{
  minUsdAmountCents: number;
  maxUsdAmountCents: number;
  onrampEnabled: boolean;
}>;

export type FundingAttempt = Readonly<{
  checkoutUrl: string;
  id: string;
  orderToken: string;
}>;

export function decodeWalletBalance(value: unknown, expectedAccount: string): WalletBalance {
  const record = object(value, "wallet balance");
  if (typeof record.account !== "string"
    || !ADDRESS.test(record.account)
    || record.account.toLowerCase() !== expectedAccount.toLowerCase()
    || typeof record.balance !== "string"
    || !INTEGER.test(record.balance)
    || record.decimals !== 6
    || record.symbol !== "MACH"
    || typeof record.token !== "string"
    || record.token.toLowerCase() !== MACHINE_USD) {
    throw new Error("The wallet balance response is invalid.");
  }
  return {
    account: record.account.toLowerCase() as `0x${string}`,
    atomics: BigInt(record.balance),
    decimals: 6,
    symbol: "MACH",
  };
}

export function formatWalletBalance(balance: WalletBalance): string {
  const scale = 1_000_000n;
  const whole = balance.atomics / scale;
  const fractional = (balance.atomics % scale).toString().padStart(6, "0").replace(/0+$/, "").padEnd(2, "0");
  return `$${whole.toLocaleString("en-US")}.${fractional || "00"}`;
}

export function decodeMachineUsdConfig(value: unknown): MachineUsdConfig {
  const record = object(value, "wallet funding config");
  const min = record.min_usd_amount_cents;
  const max = record.max_usd_amount_cents;
  const enabled = record.onramp_enabled ?? true;
  if (!Number.isSafeInteger(min) || !Number.isSafeInteger(max)
    || (min as number) <= 0 || (max as number) < (min as number)
    || typeof enabled !== "boolean"
    || record.chain_id !== TEMPO_CHAIN_ID
    || typeof record.token_address !== "string"
    || record.token_address.toLowerCase() !== MACHINE_USD
    || typeof record.stripe_publishable_key !== "string") {
    throw new Error("The Wallet funding configuration is invalid.");
  }
  return {
    minUsdAmountCents: min as number,
    maxUsdAmountCents: max as number,
    onrampEnabled: enabled,
  };
}

export function defaultFundingAmountCents(config: MachineUsdConfig): number {
  return Math.min(config.maxUsdAmountCents, Math.max(config.minUsdAmountCents, 500));
}

export function decodeFundingAttempt(
  value: unknown,
  orderToken: string,
): FundingAttempt {
  const record = object(value, "wallet funding order");
  const order = object(record.order, "wallet funding order");
  const payment = object(record.payment, "wallet funding payment");
  if (typeof order.id !== "string" || !order.id || typeof payment.checkout_url !== "string") {
    throw new Error("The Wallet funding order response is invalid.");
  }
  let checkout: URL;
  try {
    checkout = new URL(payment.checkout_url);
  } catch {
    throw new Error("The Wallet checkout URL is invalid.");
  }
  if (checkout.origin !== "https://checkout.stripe.com" || checkout.username || checkout.password) {
    throw new Error("The Wallet checkout URL is invalid.");
  }
  return { checkoutUrl: checkout.href, id: order.id, orderToken };
}

export function classifyFundingOrder(value: unknown): "complete" | "failed" | "pending" {
  const record = object(value, "wallet funding order");
  if (record.status === "complete"
    && typeof record.issuance_transaction_hash === "string"
    && TRANSACTION_HASH.test(record.issuance_transaction_hash)) {
    return "complete";
  }
  if (record.status === "failed") return "failed";
  if (record.status === "requires_payment" || record.status === "processing" || record.status === "issuing") {
    return "pending";
  }
  throw new Error("The Wallet funding order response is invalid.");
}

export function formatDollars(cents: number): string {
  return `$${(cents / 100).toLocaleString("en-US", {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`The ${label} response is invalid.`);
  }
  return value as Record<string, unknown>;
}
