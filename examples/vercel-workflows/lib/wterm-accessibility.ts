import type { WTerm } from "@wterm/dom";

export function labelWtermInput(terminal: WTerm, label: string): void {
  const input = terminal.element.querySelector("textarea");
  if (!input) return;
  input.removeAttribute("aria-hidden");
  input.setAttribute("aria-label", label);
}
