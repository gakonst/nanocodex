import type { Workspace } from "nanocodex-tools";
import type { AgentEnvironment } from "./agent-configuration";

/** At-most-once setup admission. An uncertain command must never be replayed on eviction. */
export async function prepareEnvironment(
  storage: DurableObjectStorage,
  config: AgentEnvironment,
  filesystem: Workspace,
  execute: (command: string, step: number) => Promise<{ exit_code?: number; output?: string }>,
): Promise<void> {
  const current = storage.sql.exec<{ state: string; error: string | null }>("SELECT state,error FROM managed_environment_setup").toArray()[0];
  if (current?.state === "ready") return;
  if (current) throw new Error(current.error ?? "environment setup was interrupted; recreate the session to retry");
  storage.sql.exec("INSERT INTO managed_environment_setup VALUES (1, 'running', 0, NULL)");
  let step = 0;
  try {
    for (const file of [...config.files, ...config.skills.map(skill => ({ path: `/brain/skills/${skill.name}/SKILL.md`, content: skill.instructions }))]) {
      await filesystem.writeFile(file.path, file.content);
      storage.sql.exec("UPDATE managed_environment_setup SET step=?", ++step);
    }
    for (const [commandIndex, command] of config.setup_commands.entries()) {
      const result = await execute(command, step);
      if (result.exit_code !== 0) throw new Error(`setup command ${commandIndex + 1} failed: ${result.output ?? "unknown exit status"}`);
      storage.sql.exec("UPDATE managed_environment_setup SET step=?", ++step);
    }
    storage.sql.exec("UPDATE managed_environment_setup SET state='ready'");
  } catch (error) {
    storage.sql.exec("UPDATE managed_environment_setup SET state='failed', error=?", (error instanceof Error ? error.message : String(error)).slice(0,4096));
    throw error;
  }
}
