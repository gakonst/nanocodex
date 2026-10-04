/** Existing rich conversation, including voice, Markdown, tools, queues and generated media. */
export {
  AgentTerminalView as AgentConversation,
  ConversationHistoryRail,
  ElevenLabsSettings,
} from "nanocodex-terminal";
export type {
  AgentTerminalAccessory as AgentConversationAccessory,
  AgentTerminalMode as AgentConversationMode,
  AgentTerminalState as AgentConversationState,
  ConversationSummary,
  ElevenLabsManager,
  ElevenLabsVoice,
} from "nanocodex-terminal";
export type AgentConversationProps = import("react").ComponentProps<
  typeof import("nanocodex-terminal").AgentTerminalView
>;
export type { AgentTerminalMode, AgentTerminalState, AgentTerminalAccessory } from "nanocodex-terminal";
export type { AgentStatus as AgentConversationStatus } from "nanocodex-terminal";
