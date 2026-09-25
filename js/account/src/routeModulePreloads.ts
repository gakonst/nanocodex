// Route UI and route data share an import. Keeping these loaders separate from
// repository preparation lets the home/agent entry avoid Pierre's worker code.
export async function preloadChangelog(): Promise<void> {
  const { preloadChangelog } = await import("./Changelog");
  await preloadChangelog();
}

export async function preloadDocsRoute(pathname: string): Promise<void> {
  const { preloadDocsRoute } = await import("./Docs");
  await preloadDocsRoute(pathname);
}

export async function preloadEvalOverview(): Promise<void> {
  const { preloadEvalOverview } = await import("./Evals");
  await preloadEvalOverview();
}
