import { Expiry } from "accounts";
import { parseUnits } from "viem";
import { createConfig, http } from "wagmi";
import { tempo } from "wagmi/chains";
import { tempoWallet } from "wagmi/connectors";

import { PATH_USD, USDC_E } from "./tempo-policy";

export const wagmiConfig = createConfig({
  chains: [tempo],
  connectors: [
    tempoWallet({
      accessKey: {
        authorize: () => ({
          expiry: Expiry.days(1),
          limits: [
            { token: PATH_USD, limit: parseUnits("25", 6) },
            { token: USDC_E, limit: parseUnits("25", 6) },
          ],
          showDeposit: { amount: "0.25", token: "pathUSD" },
        }),
      },
      mpp: false,
    }),
  ],
  multiInjectedProviderDiscovery: false,
  transports: { [tempo.id]: http() },
});

declare module "wagmi" {
  interface Register {
    config: typeof wagmiConfig;
  }
}
