export function fuzzyScore(value: string, query: string) {
  const candidate = value.toLowerCase();
  const needle = query.toLowerCase();
  const exactIndex = candidate.indexOf(needle);
  if (exactIndex >= 0) return 1_000 - exactIndex * 2 - candidate.length * 0.01;

  let candidateIndex = 0;
  let previousMatch = -2;
  let score = 0;
  for (const character of needle) {
    const matchIndex = candidate.indexOf(character, candidateIndex);
    if (matchIndex < 0) return null;
    const consecutive = matchIndex === previousMatch + 1;
    const boundary = matchIndex === 0 || /[\s/_.:-]/.test(candidate[matchIndex - 1]);
    score += 2 + (consecutive ? 7 : 0) + (boundary ? 4 : 0) - (matchIndex - candidateIndex) * 0.15;
    previousMatch = matchIndex;
    candidateIndex = matchIndex + 1;
  }
  return score - candidate.length * 0.005;
}
