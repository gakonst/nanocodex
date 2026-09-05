export type WorldFormationPrompt = Readonly<{
  id: string;
  label: string;
  prompt: string;
}>;

export const WORLD_FORMATION_PROMPTS: readonly WorldFormationPrompt[] = Object.freeze([
  formationPrompt(
    "triangle",
    "Triangle",
    "Everyone, form a triangle around Scout. Work out your own place together, keep the edges evenly spaced, and hold the shape.",
  ),
  formationPrompt(
    "square",
    "Square",
    "Everyone, form a square around Scout. Work out your own place together, keep the edges evenly spaced, and hold the shape.",
  ),
  formationPrompt(
    "circle",
    "Circle",
    "Everyone, form a circle around Scout. Work out your own place together, spread evenly around the ring, and hold the shape.",
  ),
  formationPrompt(
    "star",
    "Star",
    "Everyone, form a five-point star around Scout. Work out your own place together, keep the outline even, and hold the shape.",
  ),
  formationPrompt(
    "double-ring",
    "Double ring",
    "Everyone, form two rings around Scout. Work out which ring and place are yours together, spread evenly, and hold the shape.",
  ),
]);

function formationPrompt(id: string, label: string, prompt: string): WorldFormationPrompt {
  return Object.freeze({ id, label, prompt });
}
