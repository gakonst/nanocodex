import { mcpPayment, type LazyPaidMcpPayment } from "nanocodex/tempo";

/** Keep schema/crypto dependencies and method construction off chat startup. */
export function mercatorMcpPayment(
  broker: Pick<Fetcher, "fetch">, owner: string, authorize: (context: unknown) => void,
): LazyPaidMcpPayment {
  return mcpPayment(async () => {
    const { mercatorMcpPayment: createPayment } = await import("./mercator-mcp-payment-impl");
    return createPayment(broker, owner, authorize);
  });
}
