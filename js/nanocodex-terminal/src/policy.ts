export const COARSE_POINTER_QUERY = "(pointer: coarse), (any-pointer: coarse)";

export function terminalComposerAction(running: boolean, _draft: string): "send" | "stop" {
  return running ? "stop" : "send";
}
