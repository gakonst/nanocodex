export type Surface =
  | "home"
  | "agent"
  | "tools"
  | "chief-of-staff"
  | "multiplayer"
  | "world"
  | "changelog"
  | "docs"
  | "code"
  | "commits"
  | "requests"
  | "evals"
  | "connect";

export type ProductNavigationItem = Readonly<{
  surface: Surface;
  label: string;
  description: string;
}>;

const productionConnectDemoUrl = "https://nanocodex-connect-playground.gakonst.workers.dev";
const localAccountHost = /^(?:([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)\.)?nanocodex\.localhost$/;

export function connectDemoUrl(origin: string): string {
  let source: URL;
  try { source = new URL(origin); } catch { return productionConnectDemoUrl; }
  const instance = source.hostname.match(localAccountHost)?.[1];
  if (source.hostname !== "nanocodex.localhost" && !instance) return productionConnectDemoUrl;
  source.hostname = instance
    ? `${instance}.playground.nanocodex.localhost`
    : "playground.nanocodex.localhost";
  source.pathname = "/";
  source.search = "";
  source.hash = "";
  return source.href;
}

export const accountNavigation = {
  surface: "connect",
  label: "Account",
  description: "Identity & connections",
} as const satisfies ProductNavigationItem;

export const demoNavigation = [
  { surface: "agent", label: "Durable Agent", description: "Managed durable agent" },
  { surface: "chief-of-staff", label: "Chief of Staff", description: "Chat SDK channels" },
  { surface: "tools", label: "Attached Tools", description: "Browser tool host" },
  { surface: "multiplayer", label: "Multiplayer", description: "Shared room" },
  { surface: "world", label: "World", description: "Agent world" },
] as const satisfies ReadonlyArray<ProductNavigationItem>;

export const primaryNavigation = [
  { surface: "docs", label: "Docs", description: "Reference" },
  { surface: "evals", label: "Evals", description: "Benchmarks" },
] as const satisfies ReadonlyArray<ProductNavigationItem>;

export const gitNavigation = [
  { surface: "changelog", label: "Changelog", description: "Releases" },
  { surface: "commits", label: "Commits", description: "History" },
  { surface: "code", label: "Source", description: "Repository" },
] as const satisfies ReadonlyArray<ProductNavigationItem>;

const surfacePaths: Record<Surface, string> = {
  home: "/",
  agent: "/agent",
  tools: "/agent?demo=attached-tools",
  "chief-of-staff": "/demos/chief-of-staff",
  multiplayer: "/multiplayer",
  world: "/world",
  changelog: "/changelog",
  docs: "/docs",
  code: "/code",
  commits: "/commits",
  requests: "/requests",
  evals: "/evals",
  connect: "/connect",
};

const surfaces = new Set<Surface>(Object.keys(surfacePaths) as Surface[]);

export function pathForSurface(surface: Surface) {
  return surfacePaths[surface];
}

export function pathForCommit(hash: string) {
  return `${surfacePaths.commits}?${new URLSearchParams({ commit: hash })}`;
}

export function surfaceFromUrl(url: Pick<URL, "pathname" | "searchParams">): Surface {
  const pathname = url.pathname === "/" ? "/" : url.pathname.replace(/\/+$/, "");
  const legacyView = url.searchParams.get("view") as Surface | null;
  if (pathname === "/" && legacyView && surfaces.has(legacyView)) return legacyView;

  if (pathname === "/evals" || pathname.startsWith("/evals/")) return "evals";
  if (pathname === "/docs" || pathname.startsWith("/docs/")) return "docs";
  if (pathname === "/agent" && url.searchParams.get("demo") === "attached-tools") return "tools";
  if (pathname === "/connect/device") return "connect";

  const pathMatch = (Object.entries(surfacePaths) as Array<[Surface, string]>).find(
    ([, path]) => path === pathname,
  );
  if (pathMatch) return pathMatch[0];
  return legacyView && surfaces.has(legacyView) ? legacyView : "home";
}
