export type NanocodexToolsVitePlugin = Readonly<{
  name: "nanocodex-tools";
  enforce: "pre";
  resolveId(source: string, importer?: string): string | null;
}>;

/** Keeps unreachable Node-only SSH fallbacks out of browser and Worker bundles. */
export function nanocodexTools(): NanocodexToolsVitePlugin;
