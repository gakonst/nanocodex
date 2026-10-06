import { describe, expect, it } from 'vitest';
import type { ToolContext } from 'nanocodex';
import { createVaultIntakeTool } from '../src/vault-intake-tool';
// @ts-expect-error ToolRouter is a shared JavaScript runtime module.
import { ToolRouter, toolMapSource } from 'nanocodex-tools/runtime/tool-router';
describe('secure Vault intake tool', () => {
  const context = {} as ToolContext;
  const tool = createVaultIntakeTool(() => {});
  const run = (value: unknown) => tool.handler(value, context);
  it('authorizes before requesting input', () => {
    const denied = createVaultIntakeTool(() => { throw new Error('denied'); });
    expect(() => denied.handler({ kind: 'login' }, context)).toThrow('denied');
  });
  it('supports each kind without accepting credential values', () => {
    for (const kind of ['login', 'api_key', 'card', 'address', 'phone', 'totp']) {
      expect(run({ kind })).toEqual({ type: 'vault_intake', status: 'input_required', operation: 'create', kind });
      expect(() => run({ kind, password: 'secret' })).toThrow();
    }
  });
  it('routes TOTP enrollment as a value-free private form request', async () => {
    const router = new ToolRouter([toolMapSource('vault', { [tool.name]: tool })]);
    const invoke = (value: unknown) => router.snapshot().invoke(tool.name, value, context);
    expect(await invoke({ kind: 'totp', name: 'Example authenticator' })).toEqual({
      type: 'vault_intake', status: 'input_required', operation: 'create', kind: 'totp', name: 'Example authenticator',
    });
    for (const key of ['seed', 'otpauth_uri', 'code']) {
      await expect(invoke({ kind: 'totp', [key]: 'synthetic-private-material' })).rejects.toThrow();
    }
  });
  it('routes legacy website approval to a no-input receipt', async () => {
    const value = { operation: 'authorize_origin', kind: 'login', vault_id: 'a'.repeat(22), origin: 'https://example.com' };
    const router = new ToolRouter([toolMapSource('vault', { [tool.name]: tool })]);
    const result = await router.snapshot().invoke(tool.name, value, context);
    expect(result).toMatchObject({ type: 'vault_intake', status: 'not_required', operation: 'authorize_origin', kind: 'login' });
    for (const patch of [{ vault_id: 'bad' }, { kind: 'card' }, { origin: undefined }, { origin: 'http://example.com' }, { origin: 'https://example.com/' }, { origin: 'https://u:p@example.com' }, { operation: 'create' }]) expect(() => run({ ...value, ...patch })).toThrow();
  });
});
