import type { WebMcpManifest } from "./WebMcp.mjs";

export function generate(options?: {
  root?: string | undefined;
  maxFiles?: number | undefined;
  maxFileBytes?: number | undefined;
}): Promise<WebMcpManifest>;

export function validate(manifest: unknown): true;
