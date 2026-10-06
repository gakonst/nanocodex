export type HostedMachine = Readonly<{
  id: string;
  name: string;
  workspace: string;
  capabilities: readonly string[];
  resources?: HandResourceObservation;
}>;

export function normalizeHostedMachines(
  machines?: readonly HostedMachine[],
): readonly HostedMachine[];

/** Host-reported sample; bytes are host memory and the workspace filesystem, not a reservation. */
export type HandResourceObservation = Readonly<{
  observed_at_ms: number;
  cpu_logical_count?: number;
  load_average_1m?: number;
  memory_total_bytes?: number;
  memory_available_bytes?: number;
  disk_total_bytes?: number;
  disk_available_bytes?: number;
}>;
export function normalizeHandResources(value: unknown): HandResourceObservation | undefined;
