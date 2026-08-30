import type { Agent } from "../agent/index.mjs";

export { automaticWebMcpConfig as config } from "nanocodex/vite/client";

export type AutomaticNanocodexResource = Readonly<{
  data: Agent | undefined;
  error: unknown;
  status: "idle" | "pending" | "success" | "error";
  isError: boolean;
  isIdle: boolean;
  isPending: boolean;
  isSuccess: boolean;
  refetch(): void;
}>;

export function useNanocodex(options?: Readonly<{
  enabled?: boolean | undefined;
}>): AutomaticNanocodexResource;
