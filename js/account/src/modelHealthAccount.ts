type ModelHealthResource = Readonly<{
  invalidate(): void;
}>;

/** Invalidates account-scoped model readiness before crossing an identity boundary. */
export function invalidateModelHealthForAccountTransition(
  previousAccountId: string | undefined,
  accountId: string | undefined,
  resource: ModelHealthResource,
): boolean {
  if (previousAccountId === accountId) return false;
  resource.invalidate();
  return true;
}
