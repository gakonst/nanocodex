import { Expiry } from "accounts";
import { Provider } from "accounts/cli";
import { tempo } from "mppx/client";
import { Agent, Transport } from "nanocodex/node";
import { parseUnits } from "viem";
import { connect } from "viem/experimental/erc7846";
import { Actions } from "viem/tempo";
import WebSocket from "ws";
import { delegatedAccessKey, persistentChannelStore } from "./mpp-support.mjs";

class ObservedWebSocket extends WebSocket {
  constructor(...parameters) {
    super(...parameters);
    this.addEventListener("message", (event) => {
      console.error(`MPP <- ${frameSummary(event.data)}`);
    });
    this.addEventListener("close", (event) => {
      console.error(`MPP socket closed (${event.code}${event.reason ? `: ${event.reason}` : ""})`);
    });
  }

  send(data, ...parameters) {
    console.error(`MPP -> ${frameSummary(data)}`);
    return super.send(data, ...parameters);
  }

  close(code, reason) {
    console.error(`MPP socket close requested (${code ?? 1000}${reason ? `: ${reason}` : ""})`);
    return super.close(code, reason);
  }
}

const PATH_USD = "0x20c0000000000000000000000000000000000000";
const USDC_E = "0x20c000000000000000000000b9537d11c60e8b50";

const provider = Provider.create({
  mpp: false,
  open(url) {
    console.error(`Authorize Tempo account: ${url}`);
  },
  timeoutMs: 10 * 60 * 1_000,
});
await waitForHydration(provider.store);

const accessKeyStatus = await provider.getAccessKeyStatus();
const rootBeforeAuthorization = accessKeyStatus === "missing" ? undefined : provider.getAccount();
const balance = rootBeforeAuthorization
  ? await Actions.token.getBalance(provider.getClient(), {
      account: rootBeforeAuthorization,
      token: PATH_USD,
    })
  : undefined;
const needsFunding = balance === undefined || balance.amount < parseUnits("0.1", 6);
if (accessKeyStatus === "missing" || accessKeyStatus === "expired" || needsFunding) {
  await connect(provider.getClient(), {
    capabilities: {
      authorizeAccessKey: {
        expiry: Expiry.days(1),
        limits: [
          { token: PATH_USD, limit: parseUnits("25", 6) },
          { token: USDC_E, limit: parseUnits("25", 6) },
        ],
      },
      ...(needsFunding
        ? { showDeposit: { amount: "0.25", token: "pathUSD" } }
        : {}),
    },
  });
}
const rootAccount = provider.getAccount();
const account = await provider.store.accessKeys.select({
  account: rootAccount.address,
  chainId: provider.getClient().chain.id,
});
if (!account) throw new Error(`Tempo account ${rootAccount.address} has no usable access key`);
const accessKeyAddress = delegatedAccessKey(rootAccount.address, account);

const mpp = tempo.session.manager({
  account,
  autoSwap: { tokenIn: [PATH_USD], slippage: 1 },
  bootstrap: true,
  channelStore: persistentChannelStore(),
  client: provider.getClient(),
  webSocket: ObservedWebSocket,
  maxDeposit: "0.05",
  topUpAmount: "0.05",
});
let agent;
let turn;
let completed;
let watch;
let unwatch;

try {
  agent = await Agent.create({
    transport: Transport.mpp({ session: mpp }),
    thinking: "none",
    fastMode: true,
    instructions: "Answer the user's request directly and concisely.",
  });
  console.error(`Tempo root account: ${account.address}`);
  console.error(`Tempo access-key signer: ${accessKeyAddress} (${account.keyType})`);
  watch = agent.events.watch();
  unwatch = watch.onEvent((event) => {
    process.stdout.write(`${JSON.stringify(event)}\n`);
  });
  const close = process.argv.includes("--close");
  const customPrompt = process.argv.slice(2).filter((argument) => argument !== "--close").join(" ").trim();
  const prompt = customPrompt || "Reply with exactly MPP_JS_OK and nothing else.";
  turn = agent.turn.prompt({ input: prompt });
  completed = await turn.result();
  const output = completed.finalMessage;
  if (!customPrompt && output.trim() !== "MPP_JS_OK") {
    throw new Error(`unexpected model output: ${JSON.stringify(output)}`);
  }
  console.error(`Output: ${output}`);
  console.error(`Authorized cumulative payment: ${mpp.cumulative}`);
  console.error(`MPP channel: ${mpp.channelId}`);
} finally {
  try {
    completed?.dispose();
  } finally {
    turn?.dispose();
  }
  unwatch?.();
  watch?.off();
  const cleanupErrors = [];
  try {
    await agent?.session.shutdown();
  } catch (error) {
    cleanupErrors.push(error);
  }
  try {
    if (process.argv.includes("--close")) {
      const receipt = await mpp.close();
      if (receipt) {
        console.error(`Settled cumulative payment: ${receipt.acceptedCumulative}`);
        if (receipt.txHash) {
          console.error(`Settlement transaction: ${receipt.txHash}`);
          console.error(`Settlement explorer: https://explore.tempo.xyz/tx/${receipt.txHash}`);
        }
      }
    } else if (mpp.channelId) {
      console.error(`MPP channel retained for reuse: ${mpp.channelId}`);
    }
  } catch (error) {
    cleanupErrors.push(error);
  }
  if (cleanupErrors.length === 1) throw cleanupErrors[0];
  if (cleanupErrors.length > 1) {
    throw new AggregateError(cleanupErrors, "agent shutdown and MPP settlement both failed");
  }
}

async function waitForHydration(store) {
  if (store.persist.hasHydrated()) return;
  await new Promise((resolve) => {
    const timeout = setTimeout(resolve, 1_000);
    store.persist.onFinishHydration(() => {
      clearTimeout(timeout);
      resolve();
    });
  });
}

function frameSummary(data) {
  try {
    const value = JSON.parse(String(data));
    if (value.mpp === "message") {
      try {
        const application = JSON.parse(value.data);
        return `message ${application.type ?? "application"}`;
      } catch {
        return "message application";
      }
    }
    if (value.mpp === "payment-need-voucher") {
      const required = value.data?.requiredCumulative;
      return `payment-need-voucher${required === undefined ? "" : ` required=${required}`}`;
    }
    if (value.mpp === "payment-receipt") {
      const cumulative = value.data?.acceptedCumulative;
      const spent = value.data?.spent;
      return `payment-receipt${cumulative === undefined ? "" : ` cumulative=${cumulative}`}${
        spent === undefined ? "" : ` spent=${spent}`
      }`;
    }
    return typeof value.mpp === "string" ? value.mpp : "application";
  } catch {
    return "non-json";
  }
}
