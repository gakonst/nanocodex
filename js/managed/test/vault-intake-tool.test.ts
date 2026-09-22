import { describe, expect, it } from 'vitest';
import type { ToolContext } from 'nanocodex';
import { createVaultIntakeTool } from '../src/vault-intake-tool';
describe('secure Vault intake tool', () => {
  const context = {} as ToolContext;
  const tool = createVaultIntakeTool(() => {});
  const run = (value: unknown) => tool.handler(value, context);
  it('authorizes before requesting input', () => {
    const denied = createVaultIntakeTool(() => { throw new Error('denied'); });
    expect(() => denied.handler({ kind: 'login' }, context)).toThrow('denied');
  });
  it('supports each kind without accepting credential values', () => {
    for (const kind of ['login', 'api_key', 'card', 'address', 'phone']) {
      expect(run({ kind })).toEqual({ type: 'vault_intake', status: 'input_required', operation: 'create', kind });
      expect(() => run({ kind, password: 'secret' })).toThrow();
    }
  });
  it('validates existing item website approval', () => {
    const value = { operation: 'authorize_origin', kind: 'login', vault_id: 'a'.repeat(22), origin: 'https://example.com' };
    expect(run(value)).toEqual({ type: 'vault_intake', status: 'input_required', ...value });
    for (const patch of [{ vault_id: 'bad' }, { kind: 'card' }, { origin: undefined }, { origin: 'http://example.com' }, { origin: 'https://example.com/' }, { origin: 'https://u:p@example.com' }, { operation: 'create' }]) expect(() => run({ ...value, ...patch })).toThrow();
  });
});
