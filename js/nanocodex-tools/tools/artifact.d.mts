import type { NamedTool, Workspace, WorkspaceEntry } from "./types.mjs";

export const ARTIFACT_DIRECTORY: "/workspace/.nanocodex/artifacts";

export type ArtifactDocument = Readonly<{
  version: 1;
  id: string;
  title: string;
  source: string;
  createdAt: number;
  updatedAt: number;
}>;

export type ArtifactInput = Readonly<{
  id?: string | undefined;
  title: string;
  source: string;
}>;

export type ArtifactWorkspace = Readonly<{
  root: string;
  list(path?: string, options?: { maxEntries?: number }): Promise<readonly WorkspaceEntry[]>;
  readFile(path: string): Promise<Uint8Array>;
  writeFile(path: string, contents: string | ArrayBuffer | ArrayBufferView): Promise<void>;
  remove(path: string): Promise<void>;
  mkdir(path: string): Promise<void>;
}>;

export type ArtifactScan = Readonly<{
  artifacts: readonly ArtifactDocument[];
  rejected: readonly Readonly<{ path: string; error: unknown }>[];
}>;

export type ArtifactStoreOptions = Readonly<{
  directory?: string | undefined;
  validateSource?(source: string): void | Promise<void>;
}>;

export type ArtifactOptions = ArtifactStoreOptions & Readonly<{
  workspace: Pick<Workspace, "root" | "list" | "readFile" | "writeFile" | "remove" | "mkdir">;
  onArtifact?(artifact: ArtifactDocument): void;
}>;

export const artifactToolDefinition: Readonly<{
  description: string;
  parameters: Readonly<Record<string, unknown>>;
}>;

export class ArtifactStore {
  readonly directory: string;
  constructor(workspace: ArtifactWorkspace, options?: ArtifactStoreOptions);
  path(id: string): string;
  read(id: string): Promise<ArtifactDocument>;
  scan(): Promise<ArtifactScan>;
  list(): Promise<readonly ArtifactDocument[]>;
  save(input: unknown): Promise<ArtifactDocument>;
  remove(id: string): Promise<void>;
  tool(onArtifact?: (artifact: ArtifactDocument) => void): NamedTool;
}

/** Creates the named `render_artifact` tool for direct array composition. */
export function artifact(options: ArtifactOptions): NamedTool;
export function createArtifactTool(
  workspace: ArtifactWorkspace,
  onArtifact?: (artifact: ArtifactDocument) => void,
  options?: ArtifactStoreOptions,
): NamedTool;
export function artifactPath(id: string, directory?: string): string;
export function parseArtifactDocument(encoded: string): ArtifactDocument;
