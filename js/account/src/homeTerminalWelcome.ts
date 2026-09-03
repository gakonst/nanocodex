import type { CredentialSource } from "./modelSession";

const HOME_TERMINAL_WELCOME = `# High-performance Codex SDK. Runs anywhere.

\`curl -fsSL https://nanocodex.paradigm.xyz | bash\`

Rust · Node · browser WASM
One agent keeps its WebSocket, typed history, tools, and context across turns.

**Terminal-Bench 2.1 high · 82.2% · 890/890 runs**

This is the local browser agent.`;

export function homeTerminalWelcome(
  source: CredentialSource | undefined,
  freePromptsRemaining: number | null,
): string | undefined {
  if (source === undefined) return undefined;
  if (source === "brokered") {
    return `${HOME_TERMINAL_WELCOME}

This homepage demo uses your connected model account and is ephemeral: reloading discards the model thread.`;
  }
  const included = source === "sponsored" && freePromptsRemaining === 0
    ? "Your three free Luna prompts are used."
    : source === "sponsored" && freePromptsRemaining !== null
      ? `${freePromptsRemaining} of 3 free Luna prompts remain.`
      : "Verify your phone by SMS to get three free Luna prompts.";
  return `${HOME_TERMINAL_WELCOME}

${included} Free prompts use Luna without thinking and are ephemeral: reloading discards the model thread.`;
}
