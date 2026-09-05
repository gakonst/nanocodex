import assert from "node:assert/strict";
import { access, mkdir, readFile, readdir, writeFile } from "node:fs/promises";

const sourcePages = new URL("../docs/src/pages/", import.meta.url);
const navigationSource = new URL("../src/docsNavigation.ts", import.meta.url);
const client = new URL("../dist/client/", import.meta.url);
const docsOutput = new URL("docs/", client);
const pages = await markdownPages(sourcePages);
const navigation = await readFile(navigationSource, "utf8");
const routes = [...navigation.matchAll(/page\(\s*"[^"]+"\s*,\s*"(\/docs(?:\/[^"]*)?)"\s*\)/g)]
  .map((match) => match[1]);

validateSources(pages, routes);

if (process.argv.includes("--source-only")) {
  console.log(`checked ${pages.length} documentation sources`);
  process.exit(0);
}

await access(new URL("index.html", client));
const assets = await readdir(new URL("assets/", client));
const JavaScript = assets.filter((name) => name.endsWith(".js"));
const bundled = (await Promise.all(
  JavaScript.map((name) => readFile(new URL(`assets/${name}`, client), "utf8")),
)).join("\n");

assert(bundled.includes("Copy markdown"), "the native documentation surface is missing");
assert(bundled.includes("That page is not in the manual"), "the docs not-found boundary is missing");
for (const page of pages) {
  assert(bundled.includes(page.title), `the docs bundle omits ${page.route}`);
}

const byRoute = new Map(pages.map((page) => [page.route, page]));
const orderedPages = routes.map((route) => byRoute.get(route));
await mkdir(docsOutput, { recursive: true });
const index = [
  "# Nanocodex documentation",
  "",
  "A library-first Rust agents SDK with native, JavaScript, web, and retained eval consumers.",
  "",
  ...orderedPages.map(({ title, description, route }) =>
    `- [${title}](${route})${description ? ` — ${description}` : ""}`
  ),
  "",
].join("\n");
const full = orderedPages.map(({ route, source }) =>
  `Source: ${route}\n\n${rewriteLinks(stripFrontmatter(source))}`
).join("\n\n---\n\n");
await writeFile(new URL("llms.txt", docsOutput), index);
await writeFile(new URL("llms-full.txt", docsOutput), full);

function validateSources(allPages, navigationRoutes) {
  assert(navigationRoutes.length > 0, "the documentation navigation manifest is empty");
  assertUnique(navigationRoutes, "navigation route");

  const pageRoutes = allPages.map(({ route }) => route);
  assertUnique(pageRoutes, "source route");
  assert.deepEqual(
    [...navigationRoutes].sort(),
    [...pageRoutes].sort(),
    "documentation navigation and Markdown sources differ",
  );

  const byRoute = new Map(allPages.map((page) => [page.route, page]));
  const anchors = new Map(allPages.map((page) => [page.route, headingAnchors(page.body)]));
  const forbiddenPublicCopy = [
    "just build-eval-host",
    "./target/debug",
    "prepared checkout",
    "origin/master",
    "dev-georgios",
    "evals as ci",
  ];
  for (const page of allPages) {
    assert(page.title, `${page.route} is missing title frontmatter`);
    assert(page.description, `${page.route} is missing description frontmatter`);
    const h1 = headingsWithoutCode(page.body).filter(({ depth }) => depth === 1);
    assert.equal(h1.length, 1, `${page.route} must contain exactly one H1`);
    const normalizedSource = page.source.toLowerCase();
    for (const phrase of forbiddenPublicCopy) {
      assert(
        !normalizedSource.includes(phrase),
        `${page.route} contains contributor-facing public copy: ${phrase}`,
      );
    }

    for (const href of markdownLinks(page.body)) {
      if (isExternalHref(href)) continue;
      const { route, fragment } = resolveDocsHref(page.route, href);
      assert(byRoute.has(route), `${page.route} links to missing documentation route ${route}`);
      if (fragment) {
        assert(
          anchors.get(route)?.has(fragment),
          `${page.route} links to missing documentation anchor ${route}#${fragment}`,
        );
      }
    }
  }
}

async function markdownPages(directory, prefix = "") {
  const entries = await readdir(directory, { withFileTypes: true });
  const pages = await Promise.all(entries.map(async (entry) => {
    if (entry.isDirectory()) {
      return markdownPages(new URL(`${entry.name}/`, directory), `${prefix}${entry.name}/`);
    }
    if (!entry.name.endsWith(".mdx")) return [];
    const source = await readFile(new URL(entry.name, directory), "utf8");
    const stem = entry.name.slice(0, -4);
    const relative = `${prefix}${stem}`;
    return [{
      route: relative === "index" ? "/docs" : `/docs/${relative}`,
      title: frontmatter(source, "title") ?? "",
      description: frontmatter(source, "description") ?? "",
      body: stripFrontmatter(source),
      source,
    }];
  }));
  return pages.flat().sort((left, right) => left.route.localeCompare(right.route));
}

function frontmatter(source, name) {
  if (!source.startsWith("---\n")) return undefined;
  const end = source.indexOf("\n---\n", 4);
  if (end < 0) return undefined;
  return source.slice(4, end).match(new RegExp(`^${name}:\\s*(.+)$`, "m"))?.[1]
    ?.trim()
    .replace(/^(["'])(.*)\1$/, "$2");
}

function stripFrontmatter(source) {
  return source.replace(/^---\n[\s\S]*?\n---\n/, "");
}

function headingsWithoutCode(body) {
  const withoutCode = body.replace(/^```[^\n]*\n[\s\S]*?^```\s*$/gm, "");
  return [...withoutCode.matchAll(/^(#{1,3})\s+(.+)$/gm)].map((match) => ({
    depth: match[1].length,
    text: match[2].trim(),
  }));
}

function headingAnchors(body) {
  const seen = new Map();
  return new Set(headingsWithoutCode(body).map(({ text }) => {
    const base = slugify(stripInlineMarkdown(text));
    const duplicate = seen.get(base) ?? 0;
    seen.set(base, duplicate + 1);
    return duplicate ? `${base}-${duplicate + 1}` : base;
  }));
}

function markdownLinks(body) {
  const withoutCode = body.replace(/^```[^\n]*\n[\s\S]*?^```\s*$/gm, "");
  return [...withoutCode.matchAll(/\[[^\]]+\]\(([^)]+)\)/g)].map((match) => match[1]);
}

function resolveDocsHref(currentRoute, href) {
  const hash = href.indexOf("#");
  const pathname = hash >= 0 ? href.slice(0, hash) : href;
  const fragment = hash >= 0 ? decodeURIComponent(href.slice(hash + 1)) : "";
  if (!pathname) return { route: currentRoute, fragment };
  assert(pathname.startsWith("/"), `${currentRoute} uses ambiguous relative documentation link ${href}`);
  return {
    route: pathname === "/docs" || pathname.startsWith("/docs/")
      ? pathname.replace(/\/+$/, "") || "/docs"
      : `/docs${pathname}`.replace(/\/+$/, ""),
    fragment,
  };
}

function stripInlineMarkdown(value) {
  return value.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/[`*_]/g, "");
}

function slugify(value) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-") || "section";
}

function isExternalHref(href) {
  return /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href);
}

function assertUnique(values, label) {
  const duplicates = values.filter((value, index) => values.indexOf(value) !== index);
  assert.equal(duplicates.length, 0, `duplicate ${label}: ${duplicates.join(", ")}`);
}

function rewriteLinks(source) {
  return source.replace(/\]\(\/(?!docs(?:\/|\)))/g, "](/docs/");
}
