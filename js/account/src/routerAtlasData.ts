import type { RouterProvider } from "./routerApi";

export const providerNames = [
  "cloudflare",
  "openrouter",
  "vercel",
  "workers_ai",
  "chatgpt",
];
export const providerColors: Record<string, string> = {
  cloudflare: "#c78a38",
  openrouter: "#3f9b8c",
  vercel: "#8797d7",
  workers_ai: "#b58dc3",
  chatgpt: "#54a4bd",
};
export const formatMs = (n: number | null) =>
  n === null
    ? "—"
    : n >= 1000
      ? `${(n / 1000).toFixed(n >= 10000 ? 1 : 2)}s`
      : `${Math.round(n)}ms`;
// Only known catalog aliases align. Unknown names remain distinct; never combine percentiles.
export function modelFamily(model: string): string {
  if (/^(openai\/)?gpt-(6-astra|5\.6-(sol|terra|luna))$/.test(model))
    return model.replace(/^openai\//, "");
  if (["@cf/zai-org/glm-5.3", "z-ai/glm-5.3", "zai/glm-5.3"].includes(model))
    return "glm-5.3";
  return model;
}
export function atlasRows(providers: RouterProvider[]) {
  const groups = new Map<
    string,
    { key: string; model: string; effort: string; samples: RouterProvider[] }
  >();
  for (const p of providers) {
    const model = modelFamily(p.model),
      key = JSON.stringify([model, p.effort]);
    if (!groups.has(key))
      groups.set(key, { key, model, effort: p.effort, samples: [] });
    groups.get(key)!.samples.push(p);
  }
  const rank = (e: string) =>
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"].indexOf(e);
  return [...groups.values()].sort(
    (a, b) =>
      a.model.localeCompare(b.model) ||
      rank(a.effort) - rank(b.effort) ||
      a.effort.localeCompare(b.effort),
  );
}
export function globalTotals(
  providers: RouterProvider[],
  source: "live" | "probe",
) {
  return providers
    .filter((p) => p.scope === "deployment_global" && p.source === source)
    .reduce(
      (a, p) => ({
        samples: a.samples + p.sampleCount,
        failed: a.failed + p.censoredCount,
        ttft: a.ttft + p.generationTtftSampleCount,
      }),
      { samples: 0, failed: 0, ttft: 0 },
    );
}
export const isSparse = (p: RouterProvider) => p.generationTtftSampleCount < 3;
/** All cells share this labeled log(1 + milliseconds) axis, including true zero. */
export function latencyScale(providers: RouterProvider[]) {
  const max = Math.max(
    1000,
    ...providers.flatMap((p) => [
      p.generationTtftP50Ms ?? 0,
      p.generationTtftP95Ms ?? 0,
    ]),
  );
  const ceiling = 10 ** Math.ceil(Math.log10(max));
  return {
    ceiling,
    ticks: [0, 10, 100, 1000, 10000, 100000, 1000000].filter(
      (n) => n <= ceiling,
    ),
    x: (n: number) => (Math.log1p(Math.max(0, n)) / Math.log1p(ceiling)) * 100,
  };
}

/** One point per level from an explicitly selected source. Never average aliases. */
export function thinkingPoints(
  rows: ReturnType<typeof atlasRows>,
  backend: string,
  source: "live" | "probe",
) {
  return rows.map((row) => {
    const matches = row.samples.filter(
      (p) =>
        p.backend === backend &&
        p.source === source &&
        p.scope === "deployment_global",
    );
    return { row, matches, sample: matches.length === 1 ? matches[0] : null };
  });
}
