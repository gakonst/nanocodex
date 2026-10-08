export { AgentTerminalView } from "./AgentTerminalView.js";
export { ConversationHistoryRail } from "./ConversationHistoryRail.js";
export { TerminalComposer } from "./TerminalComposer.js";
export type { ComposerAttachment, ComposerAttachmentPolicy } from "./TerminalComposer.js";
export { presentAgentError } from "./errorPresentation.js";
export { GeneratedOutputView } from "./GeneratedOutputView.js";
export {
  TerminalTranscriptSurface,
  interleaveTranscriptEntries,
} from "./TerminalTranscriptSurface.js";
export { COARSE_POINTER_QUERY, TOUCH_KEYBOARD_QUERY, terminalComposerAction } from "./policy.js";
export type { AgentTerminalAccessory } from "./AgentTerminalView.js";
export type { ConversationSummary } from "./ConversationHistoryRail.js";
export type { VoiceTerminalEntry } from "./TerminalTranscriptSurface.js";
export type { AgentStatus, AgentTerminalMode, AgentTerminalState } from "./types.js";
export type { AgentEntry, ToolActivity, GeneratedOutput } from "nanocodex-react/agent";

export { ElevenLabsSettings } from "./ElevenLabsSettings.js";
export type { ElevenLabsManager, ElevenLabsVoice } from "./ElevenLabsSettings.js";
export { AgentFileProvider, logicalFilePath } from "./LinkedFile.js";
export type { AgentFileReader } from "./LinkedFile.js";
