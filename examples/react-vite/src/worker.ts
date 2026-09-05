import { Agent, Transport } from "nanocodex/host";

import {
  createExampleAgentController,
  type ExamplePayment,
} from "./agentController";
import { createPaymentSessionOwner } from "./paymentSessionOwner";
import type {
  AgentWorkerCommand,
  AgentWorkerMessage,
  StartMessage,
} from "./protocol";

type PaymentSession = Awaited<
  ReturnType<(typeof import("./tempo"))["createTempoMppSession"]>
>;

const worker = self as DedicatedWorkerGlobalScope;
const paymentSessions = createPaymentSessionOwner<PaymentSession>();
const controller = createExampleAgentController({
  createAgent,
  postMessage: (message) => worker.postMessage(message),
});
let commands = Promise.resolve();

worker.onmessage = ({ data }: MessageEvent<AgentWorkerCommand>) => {
  commands = commands
    .then(() => controller.handle(data))
    .catch((error) => {
      const message: AgentWorkerMessage = {
        type: "error",
        ...("id" in data ? { id: data.id } : {}),
        message: errorMessage(error),
      };
      worker.postMessage(message);
    });
};

async function createAgent(data: StartMessage) {
  await paymentSessions.clear();
  const common = {
    tools: {
      browserInfo: {
        description: "Return basic information about the browser Worker runtime.",
        parameters: { type: "object", additionalProperties: false },
        handler: async () => ({
          language: navigator.language,
          online: navigator.onLine,
          userAgent: navigator.userAgent,
        }),
      },
    },
    thinking: data.thinking,
    reasoningMode: data.reasoningMode,
  };
  if (data.transport === "mpp") {
    const { createTempoMppSession } = await import("./tempo");
    return paymentSessions.open(
      createTempoMppSession,
      async (paymentSession) => {
        const agent = await Agent.create({
          ...common,
          transport: Transport.mpp({ session: paymentSession.provider }),
        });
        const payment: ExamplePayment = {
          rootAddress: paymentSession.rootAddress,
          accessKeyAddress: paymentSession.accessKeyAddress,
          get channelId() {
            return paymentSession.mpp.channelId;
          },
          cumulative: () => paymentSession.mpp.cumulative.toString(),
          mcpCumulative: () => paymentSession.mcpCumulative().toString(),
        };
        return { agent, payment };
      },
    );
  }
  return {
    agent: await Agent.create({
      ...common,
    }),
  };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
