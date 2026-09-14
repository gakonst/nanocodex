import type { CredentialSource } from "./modelSession";

const HOME_TERMINAL_WELCOME = `# What should we work on?`;

export function homeTerminalWelcome(
  source: CredentialSource | undefined,
  freePromptsRemaining: number | null,
): string | undefined {
  if (source === undefined) return undefined;
  if (source === "brokered") {
    return `${HOME_TERMINAL_WELCOME}

Using your connected model account.`;
  }
  const included = source === "sponsored" && freePromptsRemaining === 0
    ? "Your three free Luna prompts are used."
    : source === "sponsored" && freePromptsRemaining !== null
      ? `${freePromptsRemaining} of 3 free Luna prompts remain.`
      : "Verify your phone by SMS to get three free Luna prompts.";
  return `${HOME_TERMINAL_WELCOME}

${included}`;
}
