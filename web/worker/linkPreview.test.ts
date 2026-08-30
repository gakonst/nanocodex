import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { test } from "node:test";

import worker from "./index.ts";
import { docsPreview } from "./docsPreview.ts";
import { routeLinkPreview } from "./linkPreview.ts";

const template = `<!doctype html><html><head>
<!-- nanocodex:link-preview:start --><title>stale</title><!-- nanocodex:link-preview:end -->
<link rel="modulepreload" href="/assets/app.js">
</head><body></body></html>`;
const deploymentSha = "d".repeat(40);

function assetEnv(assetEtag: string | null = '"asset"') {
  const requests: Request[] = [];
  return {
    env: {
      DEPLOYMENT_SHA: deploymentSha,
      ASSETS: {
        async fetch(request: Request) {
          requests.push(request);
          if (assetEtag && (request.headers.has("if-none-match") || request.headers.has("if-modified-since"))) {
            return new Response(null, { status: 304, headers: { etag: assetEtag } });
          }
          const headers = new Headers({ "content-type": "text/html" });
          if (assetEtag) headers.set("etag", assetEtag);
          return new Response(template, {
            headers,
          });
        },
      },
    },
    requests,
  };
}

test("crawler documents contain complete route-aware production metadata", async () => {
  const { env, requests } = assetEnv();
  const request = new Request("https://nanocodex-preview.workers.dev/code?path=src%2F%3Cdriver%3E.rs", {
    headers: { accept: "text/html", "user-agent": "Twitterbot/1.0" },
  });
  const response = await worker.fetch(request, env as never);
  const html = await response.text();

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/html; charset=utf-8");
  assert.equal(
    response.headers.get("cache-control"),
    "public, max-age=0, must-revalidate, no-transform",
  );
  assert.match(response.headers.get("etag") ?? "", /^"page-[0-9a-f]+"$/);
  assert.equal(
    response.headers.get("content-length"),
    String(new TextEncoder().encode(html).byteLength),
  );
  assert.equal(requests[0]?.url, "https://nanocodex-preview.workers.dev/");
  assert.match(html, /<link rel="canonical" href="https:\/\/nanocodex-preview\.workers\.dev\/code\?path=src%2F%3Cdriver%3E\.rs" \/>/);
  assert.match(html, /<meta property="og:type" content="website" \/>/);
  assert.match(html, /<meta property="og:site_name" content="Nanocodex" \/>/);
  assert.match(html, /<meta property="og:image:width" content="1200" \/>/);
  assert.match(html, /<meta property="og:image:height" content="630" \/>/);
  assert.match(html, /<meta property="og:image:type" content="image\/png" \/>/);
  assert.match(html, /<meta name="twitter:card" content="summary_large_image" \/>/);
  assert.match(
    html,
    new RegExp(`<meta name="nanocodex-deployment-sha" content="${deploymentSha}" \\/>`),
  );
  assert.match(html, /src\/&lt;driver&gt;\.rs · Nanocodex/);
  assert.doesNotMatch(html, /<driver>/);
  assert.match(html, /\/assets\/app\.js/);

  const conditional = new Request(request, {
    headers: {
      ...Object.fromEntries(request.headers),
      "if-modified-since": "Thu, 01 Jan 1970 00:00:00 GMT",
      "if-none-match": response.headers.get("etag")!,
    },
  });
  const notModified = await worker.fetch(conditional, env as never);
  assert.equal(notModified.status, 304);
  assert.equal(notModified.headers.get("etag"), response.headers.get("etag"));
  assert.equal(await notModified.text(), "");
  assert.equal(requests.length, 2);
  assert.equal(requests[1]?.headers.get("if-modified-since"), null);
  assert.equal(requests[1]?.headers.get("if-none-match"), null);
});

test("every declared document route and the internal artifact runtime retain the SPA fallback", async () => {
  const { env, requests } = assetEnv();
  const knownPaths = new Set([
    "/",
    "/agent",
    "/multiplayer",
    "/world",
    "/changelog",
    "/code",
    "/commits",
    "/requests",
    "/connect",
    "/connect/device",
    "/evals",
    ...Object.keys(docsPreview),
    "/evals/worksets/suite-one",
    "/evals/worksets/suite-one/tasks/fix-git",
  ]);
  for (const path of knownPaths) {
    const response = await worker.fetch(new Request(`https://preview.test${path}`, {
      headers: { "sec-fetch-dest": "document", "sec-fetch-mode": "navigate" },
    }), env as never);
    assert.equal(response.status, 200, path);
    assert.match(response.headers.get("content-type") ?? "", /text\/html/, path);
  }

  const releaseProbe = await worker.fetch(new Request("https://preview.test/", {
    headers: { "sec-fetch-dest": "document", "sec-fetch-mode": "cors" },
  }), env as never);
  assert.equal(releaseProbe.status, 200);
  assert.match(releaseProbe.headers.get("content-type") ?? "", /text\/html/);

  const artifact = await worker.fetch(new Request("https://preview.test/artifact-runtime?embedded=1", {
    headers: { "sec-fetch-dest": "iframe", "sec-fetch-mode": "navigate" },
  }), env as never);
  assert.equal(artifact.status, 200);
  assert.equal(artifact.headers.get("access-control-allow-origin"), "*");
  assert.match(artifact.headers.get("content-security-policy") ?? "", /default-src 'none'/);
  assert.match(artifact.headers.get("content-security-policy") ?? "", /connect-src 'none'/);
  assert.match(artifact.headers.get("content-security-policy") ?? "", /frame-ancestors 'self'/);
  const artifactHtml = await artifact.text();
  assert.match(artifactHtml, /Nanocodex — high-performance Codex SDK/);
  assert.match(artifactHtml, /\/assets\/app\.js/);
  assert.equal(requests.length, knownPaths.size + 2);
  assert.ok(requests.every((request) => new URL(request.url).pathname === "/"));
});

test("requests is a public route with canonical metadata and matching GET, HEAD, and 304 validators", async () => {
  const { env, requests } = assetEnv();
  const request = new Request("https://preview.test/requests?thread=private", {
    headers: { accept: "text/html" },
  });
  const response = await worker.fetch(request, env as never);
  const html = await response.text();
  const etag = response.headers.get("etag");

  assert.equal(response.status, 200);
  assert.match(html, /<title>Requests · Nanocodex<\/title>/);
  assert.match(html, /<link rel="canonical" href="https:\/\/preview\.test\/requests" \/>/);
  assert.match(html, /<meta property="og:url" content="https:\/\/preview\.test\/requests" \/>/);
  assert.match(html, /<meta property="og:image" content="https:\/\/preview\.test\/og\.png\?path=%2Frequests" \/>/);
  assert.doesNotMatch(html, /thread=private/);
  assert.match(etag ?? "", /^"page-[0-9a-f]+"$/);

  const head = await worker.fetch(new Request(request.url, { method: "HEAD" }), env as never);
  assert.equal(head.status, 200);
  assert.equal(head.headers.get("etag"), etag);
  assert.equal(await head.text(), "");

  const conditionalHead = await worker.fetch(new Request(request, {
    method: "HEAD",
    headers: { "if-none-match": etag!.replace(/^W\//, "") },
  }), env as never);
  assert.equal(conditionalHead.status, 304);
  assert.equal(conditionalHead.headers.get("etag"), etag);
  assert.equal(await conditionalHead.text(), "");
  assert.equal(requests.length, 3);
  assert.ok(requests.every((backing) => !backing.headers.has("if-none-match")));
});

test("genuinely unknown document routes return uncached 404s without consuming the SPA asset", async () => {
  const { env, requests } = assetEnv();
  for (const path of [
    "/definitely-not-a-route",
    "//agent",
    "/agent/child",
    "/requests/open",
  ]) {
    const response = await worker.fetch(new Request(`https://preview.test${path}`, {
      headers: { accept: "text/html", "if-none-match": '"page-deadbeef"' },
    }), env as never);
    assert.equal(response.status, 404, path);
    assert.equal(response.headers.get("cache-control"), "no-store", path);
    assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8", path);
    assert.equal(response.headers.get("etag"), null, path);
    assert.equal(await response.text(), "Not found", path);
  }

  const head = await worker.fetch(new Request("https://preview.test/not-an-app-route", {
    method: "HEAD",
    headers: { accept: "text/html" },
  }), env as never);
  assert.equal(head.status, 404);
  assert.equal(await head.text(), "");
  assert.equal(requests.length, 0);

  const scriptRequest = new Request("https://preview.test/docs.js", {
    headers: { accept: "application/javascript", "sec-fetch-dest": "script" },
  });
  assert.equal(await routeLinkPreview(scriptRequest, env as never, new URL(scriptRequest.url)), null);
});

test("missing docs and eval routes retain their client fallback with a real 404 status", async () => {
  const { env, requests } = assetEnv();
  for (const path of [
    "/docs/unknown-page",
    "/docs//getting-started",
    "/evals/worksets",
    "/evals/worksets/%E0%A4%A/tasks/run",
  ]) {
    const response = await worker.fetch(new Request(`https://preview.test${path}`, {
      headers: { accept: "text/html", "if-none-match": '"page-deadbeef"' },
    }), env as never);
    const html = await response.text();
    assert.equal(response.status, 404, path);
    assert.equal(response.headers.get("cache-control"), "no-store", path);
    assert.equal(response.headers.get("content-type"), "text/html; charset=utf-8", path);
    assert.equal(response.headers.get("etag"), null, path);
    assert.match(html, new RegExp(`property="og:url" content="https://preview\\.test${path.replaceAll("/", "\\/")}"`), path);
    assert.doesNotMatch(html, /property="og:url" content="https:\/\/preview\.test\/" \/>/, path);
  }

  const head = await worker.fetch(new Request("https://preview.test/docs/unknown-page", {
    method: "HEAD",
    headers: { accept: "text/html", "if-none-match": '"page-deadbeef"' },
  }), env as never);
  assert.equal(head.status, 404);
  assert.equal(head.headers.get("etag"), null);
  assert.equal(await head.text(), "");
  assert.equal(requests.length, 5);
  assert.ok(requests.every((backing) => !backing.headers.has("if-none-match")));
});

test("documents without an asset validator still honor the rendered page ETag", async () => {
  const { env, requests } = assetEnv(null);
  const browserRequest = new Request("https://preview.test/changelog", {
    headers: { "sec-fetch-dest": "document", "sec-fetch-mode": "navigate" },
  });
  const response = await worker.fetch(browserRequest, env as never);
  const etag = response.headers.get("etag");
  assert.equal(response.status, 200);
  assert.match(etag ?? "", /^"page-[0-9a-f]+"$/);

  const notModified = await worker.fetch(new Request(browserRequest, {
    headers: { ...Object.fromEntries(browserRequest.headers), "if-none-match": etag! },
  }), env as never);
  assert.equal(notModified.status, 304);
  assert.equal(notModified.headers.get("etag"), etag);
  assert.equal(await notModified.text(), "");
  assert.equal(requests.length, 2);
});

test("eval entity names are read safely with deterministic fallbacks", async () => {
  const values: unknown[][] = [];
  const env = {
    ...assetEnv().env,
    EVALS_DB: {
      prepare(query: string) {
        return {
          bind(...bound: unknown[]) {
            values.push(bound);
            return {
              async first() {
                return query.includes("JOIN task_definitions")
                  ? { name: "fix <unsafe> & ship", profile: "terminal-bench" }
                  : { profile: "terminal-bench", task_count: 89 };
              },
            };
          },
        };
      },
    },
  };
  const request = new Request(
    "https://preview.test/evals/worksets/suite%20one/tasks/fix%2Fgit",
    { headers: { accept: "text/html" } },
  );
  const response = await routeLinkPreview(request, env as never, new URL(request.url));
  const html = await response!.text();
  assert.deepEqual(values, [["suite one", "fix/git"]]);
  assert.match(html, /fix &lt;unsafe&gt; &amp; ship · Nanocodex/);
  assert.match(html, /retained terminal-bench treatments/);
});

test("generated PNG images are cacheable, deterministic, bounded, and conditional", async () => {
  const request = new Request("https://preview.test/og.png?path=%2Fdocs%2Fcore%2Fowned-agent");
  const response = await routeLinkPreview(request, {}, new URL(request.url));
  const bytes = new Uint8Array(await response!.arrayBuffer());
  assert.equal(response?.headers.get("content-type"), "image/png");
  assert.equal(response?.headers.get("cache-control"), "public, max-age=86400, stale-while-revalidate=604800");
  assert.deepEqual([...bytes.slice(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
  assert.ok(bytes.length > 1_000 && bytes.length < 100_000);

  const etag = response!.headers.get("etag")!;
  const conditional = new Request(request, { headers: { "if-none-match": etag } });
  const notModified = await routeLinkPreview(conditional, {}, new URL(conditional.url));
  assert.equal(notModified?.status, 304);
  assert.equal(notModified?.headers.get("etag"), etag);

  const hostile = new Request(`https://preview.test/og.png?path=${encodeURIComponent(`//evil.test/${"x".repeat(1100)}`)}`);
  const bounded = await routeLinkPreview(hostile, {}, new URL(hostile.url));
  assert.equal(bounded?.status, 200);
  assert.ok((await bounded!.arrayBuffer()).byteLength < 100_000);
});

test("Cloudflare routes every Worker-owned document and generated image through the Worker", async () => {
  const config = JSON.parse(await readFile(new URL("../wrangler.jsonc", import.meta.url), "utf8"));
  assert.deepEqual(config.assets.run_worker_first.slice(0, 18), [
    "/", "/agent", "/multiplayer", "/world", "/artifact-runtime", "/changelog",
    "/code", "/commits", "/requests", "/connect", "/connect/device", "/connect-dialog", "/connect-dialog/*",
    "/docs", "/docs/*", "/evals", "/evals/*", "/og.png",
  ]);
});

test("agent-readable documentation bypasses document rendering and reaches static assets", async () => {
  const requests: Request[] = [];
  const env = {
    ASSETS: {
      async fetch(request: Request) {
        requests.push(request);
        return new Response("Nanocodex docs", {
          headers: { "content-type": "text/plain; charset=utf-8" },
        });
      },
    },
  };
  for (const path of ["/docs/llms.txt", "/docs/llms-full.txt"]) {
    const response = await worker.fetch(new Request(`https://preview.test${path}`), env as never);
    assert.equal(response.status, 200, path);
    assert.equal(response.headers.get("content-type"), "text/plain; charset=utf-8", path);
    assert.equal(await response.text(), "Nanocodex docs", path);
  }
  assert.deepEqual(requests.map((request) => new URL(request.url).pathname), [
    "/docs/llms.txt",
    "/docs/llms-full.txt",
  ]);
});

test("the compact Worker docs projection matches every source frontmatter entry", async () => {
  const root = new URL("../docs/src/pages/", import.meta.url);
  const files = (await readdir(root, { recursive: true })).filter((file) => file.endsWith(".mdx"));
  const projected = new Set<string>();
  for (const file of files) {
    const relative = file.replace(/\.mdx$/, "");
    const route = relative === "index" ? "/docs" : `/docs/${relative.replace(/\/index$/, "")}`;
    const source = await readFile(new URL(file, root), "utf8");
    const read = (name: string) => source.match(new RegExp(`^${name}:\\s*(.+)$`, "m"))
      ?.[1]?.trim().replace(/^(["'])(.*)\1$/, "$2");
    assert.deepEqual(docsPreview[route as keyof typeof docsPreview], [read("title"), read("description")], route);
    projected.add(route);
  }
  assert.deepEqual(new Set(Object.keys(docsPreview)), projected);
});
