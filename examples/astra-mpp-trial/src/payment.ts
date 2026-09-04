import { Receipt } from "mppx";
import { Mppx, Store, tempo } from "mppx/server";
import { MACH, TEMPO_CHAIN_ID } from "./policy";

export type PaymentConfiguration = Readonly<{
  amount: "0" | "50";
  recipient: `0x${string}`;
  secret: string;
  tempoApiKey?: string;
}>;

export function paymentServer(
  storage: DurableObjectStorage,
  configuration: PaymentConfiguration,
) {
  const store = durablePaymentStore(storage);
  return Mppx.create({
    methods: [tempo.charge({
      store,
      sponsorBudget: false,
      ...(configuration.tempoApiKey
        ? { relay: { apiKey: configuration.tempoApiKey } }
        : {}),
    })],
    realm: "Nanocodex Astra one-shot",
    requiresAuth: true,
    secretKey: configuration.secret,
  });
}

export function paymentOptions(
  configuration: PaymentConfiguration,
  accountId: string,
  promptHash: string,
) {
  return {
    amount: configuration.amount,
    chainId: TEMPO_CHAIN_ID,
    currency: MACH,
    decimals: 6,
    description: "One GPT-6 Astra max-thinking prompt",
    externalId: `astra:${accountId}:${promptHash}`,
    feePayer: true,
    recipient: configuration.recipient,
    scope: paymentScope(accountId),
    supportedModes: ["pull"] as ("pull")[],
  };
}

export function paymentCredential(request: Request): string | undefined {
  const value = request.headers.get("payment-authorization")?.trim();
  return value?.replace(/^Payment\s+/i, "") || undefined;
}

export function paymentReceiptHeader(receipt: Receipt.Receipt): string {
  return Receipt.serialize(receipt);
}

export function paymentScope(accountId: string): string {
  return `astra-trial:prompt:${accountId}`;
}

function durablePaymentStore(storage: DurableObjectStorage) {
  const key = (name: string) => `mpp:${name}`;
  return Store.from({
    async get(name: string) {
      return await storage.get<unknown>(key(name)) ?? null;
    },
    async put(name: string, value: unknown) {
      await storage.put(key(name), value);
    },
    async delete(name: string) {
      await storage.delete(key(name));
    },
    async update<result>(name: string, changeValue: (current: unknown | null) => Readonly<{
      op: "noop" | "delete";
      result: result;
    }> | Readonly<{
      op: "set";
      result: result;
      value: unknown;
    }>): Promise<result> {
      return storage.transaction(async (transaction) => {
        const change = changeValue(await transaction.get<unknown>(key(name)) ?? null);
        if (change.op === "set") await transaction.put(key(name), change.value);
        if (change.op === "delete") await transaction.delete(key(name));
        return change.result;
      });
    },
  });
}
