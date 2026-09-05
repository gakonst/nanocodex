const tempoMcp = Symbol.for("nanocodex.tempo.mcp");

export const DEFAULT_MERCATOR_MCP_URL = "https://mercator.tempo.xyz/mcp";

const defaultMercator = (payment) => ({
  url: DEFAULT_MERCATOR_MCP_URL,
  description: "Discovers and composes paid Tempo services and MPP flows.",
  payment,
});

/**
 * Marks an MPP session as a Tempo provider and uses the same wallet policy for
 * Nanocodex's built-in paid Mercator MCP. Generic MPP sessions remain generic.
 */
export function createTempoProvider(options) {
  const session = options?.session;
  if (!session || typeof session.ws !== "function") {
    throw new TypeError("session must provide ws(endpoint)");
  }
  if (!options.payment || !Array.isArray(options.payment.methods) || !options.payment.methods.length) {
    throw new TypeError("Tempo provider payment must include at least one MPPx method");
  }

  const provider = {
    kind: "tempo",
    session,
    ws(endpoint) {
      return session.ws(endpoint);
    },
    ...(typeof options.fetch === "function" ? { fetch: options.fetch } : {}),
  };
  if (typeof session.close === "function") {
    provider.close = () => session.close();
  }
  Object.defineProperty(provider, tempoMcp, {
    value: { mercator: defaultMercator(options.payment) },
  });
  return Object.freeze(provider);
}

/**
 * Creates the model session and Mercator payment method from any Accounts SDK
 * provider. Accounts adapters all meet the same getMppxParameters() contract,
 * so wallet selection remains entirely application-owned.
 */
export async function createTempoProviderFromAccounts(options) {
  const wallet = options?.wallet;
  if (!wallet || typeof wallet.getMppxParameters !== "function") {
    throw new TypeError("wallet must provide getMppxParameters(options)");
  }
  const accessKey = options.accessKey;
  const walletParametersRaw = wallet.getMppxParameters(
    accessKey === undefined ? undefined : { accessKey },
  );
  if (!walletParametersRaw || typeof walletParametersRaw.getClient !== "function"
    || typeof walletParametersRaw.resolveAccount !== "function") {
    throw new TypeError("wallet.getMppxParameters() returned invalid MPPx parameters");
  }
  // Keep mppx out of normal/API-key browser bundles. It is loaded only when an
  // application explicitly asks Nanocodex to construct a Tempo wallet path.
  const [{ Mppx, tempo }, { createClient, http }] = await Promise.all([
    import("mppx/client"),
    import("viem/tempo"),
  ]);
  const walletParameters = await pinnedScopedAccountParameters(
    wallet,
    walletParametersRaw,
    accessKey,
    {
      account: options.account,
      chainId: options.chainId,
      createClient,
      http,
    },
  );
  const policy = options.policy ?? {};
  const session = tempo.session.manager({
    ...policy,
    ...options.session,
    ...walletParameters,
  });
  const method = tempo({
    ...policy,
    ...options.mercator,
    ...walletParameters,
  });
  const paymentPolicy = options.payment ?? {};
  const approvePayment = paymentApproval(paymentPolicy);
  const payment = {
    methods: [method],
    ...(approvePayment ? { onPaymentRequired: approvePayment } : {}),
  };
  const fetch = (input, init, requestPolicy) => {
    const requestApproval = paymentApproval({
      ...paymentPolicy,
      ...(requestPolicy?.maxAmount === undefined
        ? {}
        : { maxAmount: requestPolicy.maxAmount }),
    });
    const mppx = Mppx.create({
      methods: [method],
      polyfill: false,
      ...(paymentPolicy.fetch ? { fetch: paymentPolicy.fetch } : {}),
      ...(requestPolicy?.intent
        ? {
            orderChallenges(candidates) {
              return [...candidates].sort((left, right) =>
                Number(right.challenge.intent === requestPolicy.intent)
                - Number(left.challenge.intent === requestPolicy.intent));
            },
          }
        : {}),
      ...(requestApproval
        ? {
            async onChallenge(challenge, { createCredential }) {
              if (!(await requestApproval(challenge))) {
                throw new Error("MPP payment declined by Nanocodex policy.");
              }
              return createCredential();
            },
          }
        : {}),
    });
    return mppx.fetch(input, init);
  };
  return createTempoProvider({
    session,
    payment,
    fetch,
  });
}

/** @internal */
export async function pinnedScopedAccountParameters(wallet, parameters, accessKey, clientFactory) {
  const getAccessKey = wallet?.store?.accessKeys?.get;
  if (accessKey === undefined || typeof getAccessKey !== "function") return parameters;

  const state = wallet.store.getState?.();
  const rootAccount = clientFactory.account
    ?? wallet.getAccount?.()?.address
    ?? state?.accounts?.[state.activeAccount ?? 0]?.address;
  const chainId = Number(clientFactory.chainId ?? state?.chainId ?? 4217);
  if (typeof rootAccount !== "string") {
    throw new Error("The connected Accounts root address is unavailable.");
  }
  const pinnedAccount = await getAccessKey.call(wallet.store.accessKeys, {
    account: rootAccount,
    accessKey,
    chainId,
  });
  const expected = accessKey.toLowerCase();
  if (!pinnedAccount || pinnedAccount.accessKeyAddress?.toLowerCase() !== expected) {
    throw new Error(`Pinned access key "${accessKey}" is not available in the Accounts keystore.`);
  }

  const clients = new Map();

  return {
    ...parameters,
    getClient(info = {}) {
      const source = parameters.getClient(info);
      const requestedChainId = Number(info.chainId ?? source?.chain?.id ?? chainId);
      if (requestedChainId !== chainId) {
        throw new Error(
          `Pinned access key "${accessKey}" is authorized for chain ${chainId}, not ${requestedChainId}.`,
        );
      }
      let client = clients.get(requestedChainId);
      if (client) return client;

      const rpcUrl = source?.chain?.rpcUrls?.default?.http?.[0];
      if (typeof rpcUrl !== "string") {
        throw new Error(`No direct Tempo RPC URL is configured for chain ${requestedChainId}.`);
      }
      // The Accounts client routes JSON-RPC back through the wallet provider.
      // That transport may auto-select a different valid access key while
      // filling a transaction. Use the chain's direct RPC transport here, but
      // keep reads rooted at the payer account. Setting the client account to
      // the unpublished access key makes viem encode eth_call as a keychain
      // call before the first payment transaction has attached its pending
      // keyAuthorization, so even balanceOf fails with KeyNotFound. MPPx passes
      // the exact local pinnedAccount returned by resolveAccount when it
      // prepares and signs the mutating channel transaction; that is the point
      // where viem attaches and consumes the pending authorization.
      client = clientFactory.createClient({
        account: { address: rootAccount, type: "json-rpc" },
        chain: source.chain,
        transport: clientFactory.http(rpcUrl),
      });
      clients.set(requestedChainId, client);
      return client;
    },
    async resolveAccount(info) {
      const authority = info?.operation?.kind === "authorizePaymentChannel"
        ? info.operation.authority?.toLowerCase()
        : undefined;
      if (authority !== undefined && authority !== expected) {
        throw new Error(
          `MPP channel authority "${info.operation.authority}" does not match pinned access key "${accessKey}".`,
        );
      }

      // Accounts may have several valid delegated keys for the same Tempo
      // account. Its generic resolver is allowed to choose any key whose
      // scopes cover a transaction, but an MPP channel is bound to one exact
      // authority. Load that signer directly so channel top-ups and closes
      // can never drift to a newer matching key.
      if (info.chainId !== chainId) {
        throw new Error(
          `Pinned access key "${accessKey}" is authorized for chain ${chainId}, not ${info.chainId}.`,
        );
      }
      if (info.account.address.toLowerCase() !== rootAccount.toLowerCase()) {
        throw new Error(
          `Pinned access key "${accessKey}" cannot sign for account "${info.account.address}".`,
        );
      }
      return pinnedAccount;
    },
  };
}

function paymentApproval(policy) {
  const hasMaximum = policy.maxAmount !== undefined;
  const approve = policy.onPaymentRequired;
  if (!hasMaximum && typeof approve !== "function") return undefined;
  const maximum = hasMaximum ? nonNegativeBigInt(policy.maxAmount, "payment.maxAmount") : undefined;
  return async (challenge) => {
    if (maximum !== undefined) {
      const amount = challenge?.request?.amount;
      if (typeof amount !== "string" || !/^\d+$/.test(amount)) {
        throw new Error("MPP payment challenge has no valid atomic amount.");
      }
      if (BigInt(amount) > maximum) {
        throw new Error(
          `MPP payment amount ${amount} exceeds the per-request limit ${maximum}.`,
        );
      }
    }
    return typeof approve === "function" ? approve(challenge) : true;
  };
}

function nonNegativeBigInt(value, name) {
  let parsed;
  try {
    parsed = BigInt(value);
  } catch (error) {
    throw new TypeError(`${name} must be an atomic integer`, { cause: error });
  }
  if (parsed < 0n) throw new TypeError(`${name} must not be negative`);
  return parsed;
}

/** @internal */
export function resolveMcpServers(provider, configured) {
  if (configured === false) return undefined;
  const defaults = provider?.[tempoMcp];
  if (!defaults) return configured;
  return {
    ...defaults,
    ...configured,
  };
}
