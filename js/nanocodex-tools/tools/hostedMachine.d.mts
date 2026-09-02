export type HostedMachine = Readonly<{
  id: string;
  name: string;
  workspace: string;
  capabilities: readonly string[];
}>;

export function normalizeHostedMachines(
  machines?: readonly HostedMachine[],
): readonly HostedMachine[];
