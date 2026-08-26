import { Client, Dialog, Transport } from "nanocodex/connect";
import { createConfig } from "nanocodex-react/connect";

const DEFAULT_DIALOG_HOST = "https://nanocodex.gakonst.workers.dev/connect-dialog/";
const DEFAULT_API_HOST = "https://nanocodex-connect-api.gakonst.workers.dev";
const MACHINE_USD = "0x20c0000000000000000000006637932dE5413804" as const;
const USDC_E = "0x20C000000000000000000000b9537d11c60E8b50" as const;
const MACHINE_USD_SWAPPER = "0xd588ED9Ae08643A450157Adaf61c3C0C1BBd0dbb" as const;
const TIP20_CHANNEL_ESCROW = "0x4d50500000000000000000000000000000000000" as const;
const MERCATOR_SETTLEMENT = "0xa295C42FBCC026a62304A7701f25B4c91799B0dA" as const;
const localBrowserHostname = globalThis.location?.hostname === "playground.nanocodex.localhost"
  ? "nanocodex.localhost"
  : globalThis.location?.hostname.startsWith("playground-")
      && globalThis.location.hostname.endsWith(".nanocodex.localhost")
    ? globalThis.location.hostname.slice("playground-".length)
    : undefined;
const localBrowserApiHost = localBrowserHostname
  ? `${globalThis.location.protocol}//${localBrowserHostname}${
      globalThis.location.port ? `:${globalThis.location.port}` : ""
    }`
  : undefined;
export const apiHost = localBrowserApiHost
  || import.meta.env.VITE_CONNECT_API_HOST?.trim()
  || DEFAULT_API_HOST;

export const CONNECT_RESOURCES = [
  "urn:nanocodex:agent:run",
  "urn:nanocodex:capability:mercator:boost",
  "urn:nanocodex:mpp:machusd:spend",
] as const;

export const dialog = Dialog.iframe({
  host: localBrowserApiHost
    ? `${localBrowserApiHost}/connect-dialog/`
    : import.meta.env.VITE_CONNECT_DIALOG_HOST?.trim() || DEFAULT_DIALOG_HOST,
  key: "connect-playground",
  name: "Nanocodex Connect",
});

export const transport = Transport.http(apiHost, {
  key: "connect-playground",
  name: "Nanocodex Connect API",
});

export const client = Client.create({
  appId: "atlas-workspace",
  auth: {
    challenge: `${apiHost}/v1/connect/auth/challenge`,
    verify: `${apiHost}/v1/connect/auth`,
    logout: `${apiHost}/v1/connect/auth/logout`,
    resources: CONNECT_RESOURCES,
    returnToken: true,
  },
  accessKey: {
    authorize: {
      expiry: Math.floor(Date.now() / 1_000) + 30 * 86_400,
      reuse: {
        minExpiry: Math.floor(Date.now() / 1_000) + 7 * 86_400,
        minLimits: [
          { token: MACHINE_USD, limit: 10_000_000n, period: 86_400 },
          { token: USDC_E, limit: 10_000_000n, period: 86_400 },
        ],
      },
      limits: [
        { token: MACHINE_USD, limit: 10_000_000n, period: 86_400 },
        { token: USDC_E, limit: 10_000_000n, period: 86_400 },
      ],
      scopes: [
        { address: USDC_E, selector: "0xa9059cbb", recipients: [MERCATOR_SETTLEMENT] },
        { address: USDC_E, selector: "0x95777d59", recipients: [MERCATOR_SETTLEMENT] },
        { address: MACHINE_USD, selector: "0x095ea7b3", recipients: [MACHINE_USD_SWAPPER] },
        { address: MACHINE_USD_SWAPPER, selector: "0x34189fed" },
        { address: TIP20_CHANNEL_ESCROW, selector: "0xedc53b00" },
        { address: TIP20_CHANNEL_ESCROW, selector: "0xdc48471e" },
      ],
    },
  },
  dialog,
  transport,
});

export const config = createConfig({ client });
