import type { JustBashCustomCommand, JustBashDescriptor } from "../tools/bash.mjs";
import { justBash } from "../tools/bash.mjs";
import type { NamedTool, Workspace } from "../tools/types.mjs";

import { createGhCommand, createGitCommand, type ShellFetch } from "./shell.js";

const DEFAULT_MAX_ENTRIES = 20_000;
const DEFAULT_MAX_OUTPUT_TOKENS = 10_000;

export type ComputerCommandContext = Readonly<{
  fetch: ShellFetch;
  filesystem(): Workspace;
}>;

export type ComputerRuntimeOptions = Readonly<{
  filesystem: Workspace;
  fetch: ShellFetch;
  networkMode: string;
  maxEntries?: number | undefined;
  maxOutputTokens?: number | undefined;
  commands?(context: ComputerCommandContext): readonly JustBashCustomCommand[];
}>;

export type ComputerRuntime = Readonly<{
  commandNames: readonly string[];
  descriptor: JustBashDescriptor;
  fetch: ShellFetch;
  filesystem: Workspace;
  instructions: string;
  tool: NamedTool;
}>;

/**
 * Mounts persistent storage into the bounded JS shell and installs the generic
 * Git/GitHub compatibility commands. Hosts inject all network and extra command
 * capabilities; this package owns no credentials or provider policy.
 */
export async function createComputerRuntime(
  options: ComputerRuntimeOptions,
): Promise<ComputerRuntime> {
  let mountedFilesystem: Workspace | undefined;
  const filesystem = () => {
    if (!mountedFilesystem) throw new Error("computer filesystem is not mounted");
    return mountedFilesystem;
  };
  const git = createGitCommand(options.fetch, filesystem);
  const gh = createGhCommand(options.fetch, (args, context) => git.execute(args, context));
  const additional = options.commands?.({ fetch: options.fetch, filesystem }) ?? [];
  const shell = await justBash({
    filesystem: options.filesystem,
    maxEntries: options.maxEntries ?? DEFAULT_MAX_ENTRIES,
    maxOutputTokens: options.maxOutputTokens ?? DEFAULT_MAX_OUTPUT_TOKENS,
    fetch: options.fetch,
    networkMode: options.networkMode,
    customCommands: [git, gh, ...additional],
  });
  mountedFilesystem = shell.filesystem;
  return Object.freeze({
    commandNames: shell.descriptor.customCommands,
    descriptor: shell.descriptor,
    fetch: options.fetch,
    filesystem: shell.filesystem,
    instructions: shell.instructions,
    tool: shell.tool,
  });
}
