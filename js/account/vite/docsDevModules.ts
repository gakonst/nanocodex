import { fileURLToPath } from "node:url";

const docsModulePrefix = "/docs/src/pages/";
const docsSourceRoot = new URL("../docs/src/pages/", import.meta.url);

export function rewriteDocsDevModuleUrl(requestUrl: string | undefined) {
  if (requestUrl == null) return undefined;
  let url: URL;
  try {
    url = new URL(requestUrl, "https://localhost");
  } catch {
    return undefined;
  }
  if (!url.pathname.startsWith(docsModulePrefix) || !url.searchParams.has("raw")) {
    return undefined;
  }

  const encodedRelative = url.pathname.slice(docsModulePrefix.length);
  let relative: string;
  try {
    relative = decodeURIComponent(encodedRelative);
  } catch {
    return undefined;
  }
  if (
    !relative.endsWith(".mdx") ||
    relative.startsWith("/") ||
    relative.split("/").some((segment) => segment === "" || segment === "." || segment === "..")
  ) return undefined;

  const absolute = fileURLToPath(new URL(relative, docsSourceRoot));
  return `/@fs${absolute}${url.search}`;
}
