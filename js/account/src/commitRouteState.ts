export function commitPreparationMatchesIntent(
  preparedCommit: string | undefined,
  intendedCommit: string | undefined,
): boolean {
  return preparedCommit === intendedCommit;
}

type RepositoryNavigationIntent<T> = {
  navigationId: number;
  latestNavigationId(): number;
  preparation: Promise<T>;
  onPrepared(prepared: T): void;
  onFailure(): void;
  navigate(): void;
};

export async function settleRepositoryNavigationIntent<T>({
  navigationId,
  latestNavigationId,
  preparation,
  onPrepared,
  onFailure,
  navigate,
}: RepositoryNavigationIntent<T>): Promise<"ready" | "failed" | "stale"> {
  let prepared: T;
  try {
    prepared = await preparation;
  } catch {
    if (latestNavigationId() !== navigationId) return "stale";
    onFailure();
    if (latestNavigationId() !== navigationId) return "stale";
    navigate();
    return "failed";
  }
  if (latestNavigationId() !== navigationId) return "stale";
  onPrepared(prepared);
  if (latestNavigationId() !== navigationId) return "stale";
  navigate();
  return "ready";
}
