export function executeNativePatch(patch: string, host: {
  readWorkspaceFile(path: string): Promise<Uint8Array>;
  writeWorkspaceFile(path: string, bytes: Uint8Array): Promise<void>;
  removeWorkspaceFile(path: string): Promise<void>;
}): Promise<string>;
