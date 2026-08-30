import type { WebMcpManifest } from "./WebMcp.mjs";

export function generate(options?: {
  root?: string | undefined;
  exclude?: readonly string[] | undefined;
  maxFiles?: number | undefined;
  maxFileBytes?: number | undefined;
}): Promise<WebMcpManifest>;

export type GenerateWebMcpManifestOptions = Readonly<{
  root?: string | undefined;
  output?: string | undefined;
  maxFiles?: number | undefined;
  maxFileBytes?: number | undefined;
}>;

export function generateFile(options?: GenerateWebMcpManifestOptions): Promise<Readonly<{
  changed: boolean;
  manifest: WebMcpManifest;
  path: string;
}>>;

export function validate(manifest: unknown): true;
