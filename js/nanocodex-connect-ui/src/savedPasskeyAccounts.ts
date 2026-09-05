export type SavedPasskeyAccount = Readonly<{
  address: `0x${string}`;
  credential?: Readonly<{ id: string }> | undefined;
  label?: string | undefined;
}>;

export type SavedPasskeyStore<Account extends SavedPasskeyAccount = SavedPasskeyAccount> = {
  getState(): { accounts: readonly Account[] };
  setState(state: { accounts: readonly Account[] }): unknown;
};

export async function retainSavedPasskeyLabels<
  Result,
  Account extends SavedPasskeyAccount,
>(
  store: SavedPasskeyStore<Account>,
  request: () => Promise<Result>,
): Promise<Result> {
  const saved = store.getState().accounts;
  const result = await request();
  const state = store.getState();
  let changed = false;
  const accounts = state.accounts.map((account) => {
    const credentialId = account.credential?.id;
    if (!credentialId) return account;
    const prior = saved.find((candidate) => candidate.credential?.id === credentialId);
    if (!prior?.label
      || prior.label === account.label
      || (account.label && account.label !== "Saved passkey")) return account;
    changed = true;
    return { ...account, label: prior.label };
  });
  if (changed) store.setState({ accounts });
  return result;
}
