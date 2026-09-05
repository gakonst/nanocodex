export type VaultEgressEnvelope = Readonly<{
  vault_id: string;
  url: string;
  method: string;
  headers: Record<string, string>;
  body?: string;
}>;

export function vaultEgressEnvelope(value: unknown): VaultEgressEnvelope | undefined;
