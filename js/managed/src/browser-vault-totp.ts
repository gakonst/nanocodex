import type { ToolContext } from "nanocodex";
import { captureBrowserVaultBinding, fillBrowserVaultOtp, type BrowserVaultIdentity, type PrivateBrowserCdp } from "./browser-vault";

/** Private broker RPC only. Neither the seed nor its generated code is a tool result. */
export type BrowserVaultTotpResolver = (
  request: Readonly<{ totp_vault_id: string; expected_origin: string }>, context: ToolContext,
) => Promise<string>;

/** Called inside the runtime's selected-session quarantine and durable operation fence. */
export async function fillSavedBrowserVaultTotp(options: {
  cdp: PrivateBrowserCdp;
  identity: BrowserVaultIdentity;
  resolve: () => Promise<string>;
  remember: (code: string) => Promise<void>;
  signal: AbortSignal;
}): Promise<{ status: "submitted" }> {
  const binding = await captureBrowserVaultBinding(options.cdp, options.identity);
  const code = await options.resolve();
  if (typeof code !== "string" || !/^(?:[0-9]{6}|[0-9]{8})$/.test(code)) throw new Error("Invalid private Vault response");
  // Register the secret and durable lost-memory guard before any page interaction.
  await options.remember(code);
  await fillBrowserVaultOtp({ cdp: options.cdp,
    request: { ...options.identity, otp_selector: binding.otp_selector, expected_loader_id: binding.loaderId },
    resolve: async () => code, submit: true, signal: options.signal });
  return { status: "submitted" };
}
