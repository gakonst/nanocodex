/**
 * @internal Hook installed by `mcpPayment()` from `nanocodex/tempo`. Core MCP
 * code calls it to wrap a paid server's client without importing mppx itself.
 */
export const mcpPaymentWrap = Symbol.for("nanocodex.mcp.payment.wrap");

// Module-private identity: only the SDK factory creates lazy payment values.
export const mcpPaymentFactory = Symbol("nanocodex.mcp.payment.factory");
