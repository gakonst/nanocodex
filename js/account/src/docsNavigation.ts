export type DocsPage = {
  type: "page";
  label: string;
  href: string;
};

export type DocsSection = {
  type: "section";
  id: string;
  label: string;
  pages: readonly DocsPage[];
};

export type DocsNavigationItem = DocsPage | DocsSection;

export type DocsNavigationGroup = {
  label: string;
  items: readonly DocsNavigationItem[];
};

const page = (label: string, href: string): DocsPage => ({
  type: "page",
  label,
  href,
});

export const docsNavigation: readonly DocsNavigationGroup[] = [
  {
    label: "USING NANOCODEX",
    items: [
      page("Quick start", "/docs"),
      page("Installation and auth", "/docs/getting-started"),
    ],
  },
  {
    label: "EVALUATING NANOCODEX",
    items: [
      page("Overview", "/docs/evals"),
      page("Use the eval harness", "/docs/harness/focused-run"),
      page("Dashboard and worksets", "/docs/harness/dashboard-worksets"),
      page("Runs and evidence", "/docs/harness/evidence"),
      page("Interpret results", "/docs/harness/read-results"),
    ],
  },
  {
    label: "EMBEDDING NANOCODEX",
    items: [
      {
        type: "section",
        id: "typescript-sdk",
        label: "JavaScript / TypeScript",
        pages: [
          page("Overview", "/docs/sdks/javascript"),
          page("Install from npm", "/docs/sdks/javascript/install-entrypoints"),
          page("Agent lifecycle", "/docs/sdks/javascript/agent-lifecycle"),
          page("Transports and auth", "/docs/sdks/javascript/transports-auth"),
          page("Browser WASM and workspace", "/docs/sdks/javascript/browser-workspace"),
          page("React", "/docs/sdks/javascript/react"),
        ],
      },
      {
        type: "section",
        id: "deployments",
        label: "Deployments",
        pages: [
          page("Choose a deployment", "/docs/deployments"),
          page("Cloudflare Durable Objects", "/docs/deployments/cloudflare"),
          page("Vercel Workflows", "/docs/deployments/vercel"),
        ],
      },
      {
        type: "section",
        id: "rust-sdk",
        label: "Rust SDK",
        pages: [
          page("Overview", "/docs/sdks/rust"),
          page("Agent lifecycle", "/docs/sdks/rust/agent-lifecycle"),
          page("Turns, events, and control", "/docs/sdks/rust/turns-events-control"),
          page("Owned session model", "/docs/core/owned-agent"),
        ],
      },
      page("Python", "/docs/sdks/python"),
    ],
  },
  {
    label: "ADVANCED FUNCTIONALITY",
    items: [
      {
        type: "section",
        id: "advanced-tools",
        label: "Tools and Code Mode",
        pages: [
          page("Tools, Code Mode, and subagents", "/docs/sdks/javascript/tools-code-mode-subagents"),
          page("Rust tools, Code Mode, and MCP", "/docs/sdks/rust/tools-code-mode-mcp"),
          page("How Code Mode works", "/docs/core/tools-code-mode"),
        ],
      },
      {
        type: "section",
        id: "branches-durability",
        label: "Branches and durability",
        pages: [
          page("Rust forks and subagents", "/docs/sdks/rust/handles-forks-subagents"),
          page("Branching and subagents", "/docs/core/branching"),
          page("Rust durability", "/docs/sdks/rust/durability"),
          page("Durable execution", "/docs/core/durability"),
        ],
      },
      {
        type: "section",
        id: "advanced-capabilities",
        label: "Browser and other capabilities",
        pages: [
          page("Browser-local agent", "/docs/capabilities/web-agent"),
          page("VMs and sandboxes", "/docs/capabilities/vm-sandboxes"),
          page("Voice", "/docs/capabilities/voice"),
          page("Built with Nanocodex: Tact", "/docs/examples/tact"),
        ],
      },
      page("Stability and scope", "/docs/stability"),
    ],
  },
] as const;

export const docsPageOrder: readonly DocsPage[] = docsNavigation.flatMap(({ items }) =>
  items.flatMap((item) => item.type === "page" ? [item] : [...item.pages]),
);

const sourceModules = import.meta.glob("../docs/src/pages/**/*.mdx", {
  import: "default",
  query: "?raw",
}) as Record<string, () => Promise<string>>;

const sourceLoaders = new Map(
  Object.entries(sourceModules).map(([file, load]) => [routeForSource(file), load]),
);
const sourceCache = new Map<string, Promise<string>>();

export function hasDocsSource(path: string) {
  return sourceLoaders.has(path);
}

export function loadDocsSource(path: string): Promise<string | undefined> {
  const load = sourceLoaders.get(path);
  if (!load) return Promise.resolve(undefined);

  const cached = sourceCache.get(path);
  if (cached) return cached;

  const pending = load().catch((error: unknown) => {
    sourceCache.delete(path);
    throw error;
  });
  sourceCache.set(path, pending);
  return pending;
}

export function normalizeDocsPath(pathname: string) {
  const path = pathname.replace(/\/+$/, "");
  return path || "/docs";
}

function routeForSource(file: string) {
  const relative = file.split("/pages/")[1].replace(/\.mdx$/, "");
  return relative === "index" ? "/docs" : `/docs/${relative}`;
}
