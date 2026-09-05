import { Provider, Storage } from "accounts";
import { createJsonChannelStore } from "mppx/client";
import { createTempoProviderFromAccounts } from "nanocodex/host";
import { parseUnits } from "viem";
import { PATH_USD, USDC_E } from "./tempo-policy";

const accessKeyLimits = [
  { token: PATH_USD, limit: parseUnits("25", 6) },
  { token: USDC_E, limit: parseUnits("25", 6) },
];

type AccessKeyRecord = {
  address: `0x${string}`;
  limits?: readonly { token: `0x${string}`; limit: bigint }[];
};

type AccountsStore = {
  accessKeys: {
    get(options: {
      account: `0x${string}`;
      accessKey: `0x${string}`;
      chainId: number;
    }): Promise<unknown>;
    list(query: { account: `0x${string}`; chainId: number }): readonly AccessKeyRecord[];
  };
  persist: {
    hasHydrated(): boolean;
    onFinishHydration(listener: () => void): () => void;
  };
};

type AccountsProvider = Omit<Provider.Provider, "store"> & { store: AccountsStore };

export async function createTempoMppSession() {
  const provider = Provider.create({ mpp: false, storage: Storage.idb() }) as unknown as AccountsProvider;
  await waitForHydration(provider.store);
  const root = provider.getAccount();
  const record = findReusableAccessKey(provider, root.address);
  if (!record) throw new Error("Authorize the Tempo access key in the page first");

  const channelStorage = Storage.idb({ key: "nanocodex-mpp-channels" });
  const channelStore = createJsonChannelStore({
    async get(key) {
      return (await channelStorage.getItem<string>(key)) ?? undefined;
    },
    async set(key, value) {
      await channelStorage.setItem(key, value);
    },
    async delete(key) {
      await channelStorage.removeItem(key);
    },
  });
  const mcpChannels = new Map<string, bigint>();
  const tempoProvider = await createTempoProviderFromAccounts({
    wallet: provider,
    accessKey: record.address,
    policy: {
      autoSwap: { tokenIn: [PATH_USD as `0x${string}`], slippage: 1 },
      channelStore,
      maxDeposit: "0.05",
      topUpAmount: "0.05",
    },
    session: { bootstrap: true },
    mercator: {
      onChannelUpdate(entry) {
        mcpChannels.set(entry.channelId, entry.cumulativeAmount);
      },
    },
  });
  return {
    mpp: tempoProvider.session,
    provider: tempoProvider,
    mcpCumulative: () => [...mcpChannels.values()].reduce((total, value) => total + value, 0n),
    rootAddress: root.address,
    accessKeyAddress: record.address,
  };
}

function findReusableAccessKey(provider: AccountsProvider, rootAddress: string) {
  return provider.store.accessKeys
    .list({ account: rootAddress as `0x${string}`, chainId: provider.getClient().chain.id })
    .find((key) =>
      accessKeyLimits.every((required) =>
        key.limits?.some(
          (limit) =>
            limit.token.toLowerCase() === required.token.toLowerCase() &&
            limit.limit >= required.limit,
        ),
      ),
    );
}

async function waitForHydration(store: AccountsStore) {
  if (store.persist.hasHydrated()) return;
  await new Promise<void>((resolve) => {
    const timeout = setTimeout(resolve, 1_000);
    store.persist.onFinishHydration(() => {
      clearTimeout(timeout);
      resolve();
    });
  });
}
