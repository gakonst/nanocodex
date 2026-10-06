import type { VaultEntry } from './broker';

/** Host-only materialization. Never forward this object through the model gateway. */
export const VAULT_FIELDS = {
  login: ['username', 'password'],
  api_key: ['api_key'],
  card: ['card_number', 'expiry_month', 'expiry_year', 'card_expiry', 'cvv', 'billing_zip'],
  address: ['address_line_1', 'address_line_2', 'city', 'state', 'zip', 'country'],
  phone: ['phone_number'],
} as const;
export type VaultField = typeof VAULT_FIELDS[keyof typeof VAULT_FIELDS][number];
export function validVaultFields(value: unknown): value is VaultField[] {
  return Array.isArray(value) && value.length > 0 && value.length <= 32 && new Set(value).size === value.length
    && value.every(field => typeof field === 'string' && Object.values(VAULT_FIELDS).some(fields => (fields as readonly string[]).includes(field)));
}
export function materializeVaultFields(entry: VaultEntry, fields: readonly VaultField[]): Record<string, string> {
  if (fields.some(field => !(VAULT_FIELDS[entry.kind] as readonly string[]).includes(field))) throw new Error('Vault field kind mismatch');
  if (fields.includes('cvv') && entry.kind === 'card' && !entry.cvv) throw new Error('Vault field unavailable');
  return Object.fromEntries(fields.map(field => [field, field === 'card_expiry' && entry.kind === 'card'
    ? `${entry.expiry_month.padStart(2, '0')}/${entry.expiry_year}`
    : (entry as unknown as Record<string, string>)[field] ?? '']));
}
