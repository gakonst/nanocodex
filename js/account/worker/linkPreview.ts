import { docsPreview } from "./docsPreview.ts";

const SITE_NAME = "Nanocodex";
const IMAGE_WIDTH = 1200;
const IMAGE_HEIGHT = 630;
const METADATA_START = "<!-- nanocodex:link-preview:start -->";
const METADATA_END = "<!-- nanocodex:link-preview:end -->";
const AGENT_DOC_PATHS = new Set(["/docs/llms.txt", "/docs/llms-full.txt"]);
const GIT_SHA_PATTERN = /^[0-9a-f]{40}$/;
const ARTIFACT_RUNTIME_CSP = [
  "default-src 'none'",
  "script-src 'self' 'unsafe-eval'",
  "style-src 'self' 'unsafe-inline'",
  "img-src data: blob:",
  "connect-src 'none'",
  "font-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'self'",
].join("; ");

type LinkPreviewEnv = {
  ASSETS?: Fetcher;
  DEPLOYMENT_SHA?: string;
  EVALS_DB?: D1Database;
};

type Preview = {
  canonicalPath: string;
  description: string;
  eyebrow: string;
  title: string;
};

export async function routeLinkPreview(
  request: Request,
  env: LinkPreviewEnv,
  url: URL,
): Promise<Response | null> {
  if (
    AGENT_DOC_PATHS.has(url.pathname)
    && (request.method === "GET" || request.method === "HEAD")
    && env.ASSETS
  ) {
    return env.ASSETS.fetch(request);
  }
  if (url.pathname === "/og.png" && (request.method === "GET" || request.method === "HEAD")) {
    return previewImage(request, env, url);
  }
  const pathname = normalizePath(url.pathname);
  const documentStatus = documentStatusForPath(pathname);
  const internalNavigation = pathname === "/artifact-runtime" && isIframeNavigation(request);
  const routeHead = request.method === "HEAD" && documentStatus != null;
  const genericKnownDocument = documentStatus != null && isGenericDocumentGet(request);
  if (
    (!isDocumentNavigation(request) && !genericKnownDocument && !internalNavigation && !routeHead)
    || !env.ASSETS
  ) return null;
  if (documentStatus == null) return documentNotFound(request);

  const preview = await previewForUrl(url, env);
  const assetHeaders = new Headers(request.headers);
  assetHeaders.delete("if-modified-since");
  assetHeaders.delete("if-none-match");
  const assetResponse = await env.ASSETS.fetch(new Request(new URL("/", url), {
    headers: assetHeaders,
    method: "GET",
  }));
  if (!assetResponse.ok) return assetResponse;

  const headers = new Headers(assetResponse.headers);
  headers.set("cache-control", "public, max-age=0, must-revalidate, no-transform");
  headers.set("content-type", "text/html; charset=utf-8");
  headers.set("x-content-type-options", "nosniff");
  headers.delete("content-encoding");
  headers.delete("content-length");
  if (pathname === "/artifact-runtime") {
    headers.set("access-control-allow-origin", "*");
    headers.set("content-security-policy", ARTIFACT_RUNTIME_CSP);
  }
  if (documentStatus === 404) {
    headers.set("cache-control", "no-store");
    headers.delete("etag");
    headers.delete("last-modified");
    const html = request.method === "HEAD"
      ? null
      : await renderLinkPreviewDocument(await assetResponse.text(), url, env, preview);
    return new Response(html, { headers, status: 404 });
  }

  const html = await renderLinkPreviewDocument(await assetResponse.text(), url, env, preview);
  const etag = pageEtag(fnv1a(html));
  headers.set("etag", etag);
  headers.set("content-length", String(new TextEncoder().encode(html).byteLength));
  if (etagMatches(request.headers.get("if-none-match"), etag)) {
    return new Response(null, { headers, status: 304 });
  }
  return new Response(request.method === "HEAD" ? null : html, {
    headers,
    status: assetResponse.status,
  });
}

export async function renderLinkPreviewDocument(
  document: string,
  url: URL,
  env: Pick<LinkPreviewEnv, "DEPLOYMENT_SHA" | "EVALS_DB"> = {},
  resolved?: Preview,
): Promise<string> {
  const preview = resolved ?? await previewForUrl(url, env);
  const origin = url.origin;
  const canonicalUrl = new URL(preview.canonicalPath, origin).href;
  const imagePath = new URL("/og.png", origin);
  imagePath.searchParams.set("path", preview.canonicalPath);
  return injectMetadata(
    document,
    metadataHtml(preview, canonicalUrl, imagePath.href, env.DEPLOYMENT_SHA),
  );
}

export function isDocumentNavigation(request: Request): boolean {
  if (request.method !== "GET" && request.method !== "HEAD") return false;
  const destination = request.headers.get("sec-fetch-dest");
  if (destination) return destination === "document";
  const mode = request.headers.get("sec-fetch-mode");
  if (mode) return mode === "navigate";
  return request.headers.get("accept")?.toLowerCase().includes("text/html") === true;
}

function isIframeNavigation(request: Request): boolean {
  return (request.method === "GET" || request.method === "HEAD")
    && request.headers.get("sec-fetch-dest") === "iframe";
}

export function documentStatusForPath(pathname: string): 200 | 404 | null {
  pathname = normalizePath(pathname);
  if (pathname === "/" || pathname === "/agent" || isAgentDocumentPath(pathname)
    || pathname === "/multiplayer"
    || pathname === "/world" || pathname === "/artifact-runtime"
    || pathname === "/demos/chief-of-staff"
    || pathname === "/changelog" || pathname === "/code" || pathname === "/commits"
    || pathname === "/requests" || pathname === "/connect"
    || pathname === "/connect/device" || pathname === "/connect/vault") return 200;
  if (Object.hasOwn(docsPreview, pathname) || isEvalDocumentPath(pathname)) return 200;
  if (pathname.startsWith("/docs/") || pathname.startsWith("/evals/")) return 404;
  return null;
}

function isEvalDocumentPath(pathname: string): boolean {
  const segments = pathname.split("/").filter(Boolean);
  if (segments.length === 1 && segments[0] === "evals") return true;
  if (segments.length === 3 && segments[0] === "evals" && segments[1] === "worksets") {
    return decodeSegment(segments[2]) != null;
  }
  return segments.length === 5 && segments[0] === "evals" && segments[1] === "worksets"
    && segments[3] === "tasks" && decodeSegment(segments[2]) != null
    && decodeSegment(segments[4]) != null;
}

function documentNotFound(request: Request): Response {
  return new Response(request.method === "HEAD" ? null : "Not found", {
    status: 404,
    headers: {
      "cache-control": "no-store",
      "content-type": "text/plain; charset=utf-8",
      "x-content-type-options": "nosniff",
    },
  });
}

async function previewImage(request: Request, env: LinkPreviewEnv, url: URL): Promise<Response> {
  const path = boundedPreviewPath(url.searchParams.get("path"));
  const previewUrl = new URL(path, url);
  const preview = await previewForUrl(previewUrl, env);
  const identity = `${preview.eyebrow}\n${preview.title}\n${preview.description}`;
  const etag = `"og-${fnv1a(identity)}"`;
  const headers = new Headers({
    "cache-control": "public, max-age=86400, stale-while-revalidate=604800",
    "content-type": "image/png",
    etag,
    "x-content-type-options": "nosniff",
  });
  if (request.headers.get("if-none-match") === etag) {
    return new Response(null, { headers, status: 304 });
  }
  if (request.method === "HEAD") return new Response(null, { headers });
  const png = await renderPng(preview);
  return new Response(png.buffer as ArrayBuffer, { headers });
}

function pageEtag(hash: string): string {
  // no-transform keeps the final bytes stable across the edge, so clients can
  // use a strong validator without redownloading the application shell.
  return `"page-${hash}"`;
}

function etagMatches(value: string | null, etag: string): boolean {
  if (!value) return false;
  const expected = etag.replace(/^W\//, "");
  return value.split(",").some((candidate) => {
    const normalized = candidate.trim();
    return normalized === "*" || normalized.replace(/^W\//, "") === expected;
  });
}

function isGenericDocumentGet(request: Request): boolean {
  if (request.method !== "GET" && request.method !== "HEAD") return false;
  if (request.headers.has("sec-fetch-dest") || request.headers.has("sec-fetch-mode")) return false;
  const accept = request.headers.get("accept")?.toLowerCase();
  return accept == null || accept.includes("*/*");
}

async function previewForUrl(url: URL, env: LinkPreviewEnv): Promise<Preview> {
  const pathname = normalizePath(url.pathname);
  if (pathname === "/") {
    return {
      canonicalPath: "/",
      description: "High-performance Codex SDK. Runs anywhere. Terminal-Bench 2.1 high: 82.2% vs Codex 79.6%; 890/890 runs.",
      eyebrow: "HIGH-PERFORMANCE CODEX SDK · RUNS ANYWHERE",
      title: "Nanocodex",
    };
  }
  if (pathname === "/agent" || isAgentDocumentPath(pathname)) {
    return fixed(pathname, "Durable agent", "Open an account-owned durable Nanocodex agent.");
  }
  if (pathname === "/demos/chief-of-staff") return fixed(pathname, "Chief of Staff", "Connect a signed Slack ingress to account-owned durable Nanocodex agents.", "CHAT SDK INTEGRATION");
  if (pathname === "/multiplayer") return fixed(pathname, "Multiplayer", "Join a durable room with many humans and one secretless managed Nanocodex agent.", "DURABLE MULTIPLAYER");
  if (pathname === "/world") return fixed(pathname, "Springleaf Town", "Watch Nanocodex inhabitants act inside a living pixel world.", "MONSTER WORLD");
  if (pathname === "/changelog") return fixed(pathname, "Changelog", "Follow focused Nanocodex SDK, runtime, tooling, and evaluation changes.");
  if (pathname === "/commits") return fixed(pathname, "Commits", "Inspect the published Nanocodex source history and focused patches.");
  if (pathname === "/requests") return fixed(pathname, "Requests", "Track proposed changes to the published Nanocodex source tree.", "REQUESTS");
  if (pathname === "/connect") return fixed(pathname, "Connect", "Manage your Nanocodex identity, connections, and API keys.", "NANOCODEX CONNECT");
  if (pathname === "/connect/device") return fixed(pathname, "Connect device", "Authorize a Nanocodex device with your passkey-backed account.", "NANOCODEX CONNECT");
  if (pathname === "/connect/vault") return fixed(pathname, "Vault", "Store encrypted SSH keys, logins, API keys, cards, addresses, and phone numbers.", "NANOCODEX CONNECT");
  if (pathname === "/code") {
    const sourcePath = boundedText(url.searchParams.get("path"), 240);
    const canonical = new URL("https://canonical.invalid/code");
    if (sourcePath) canonical.searchParams.set("path", sourcePath);
    return {
      canonicalPath: `${canonical.pathname}${canonical.search}`,
      description: sourcePath
        ? `Read ${sourcePath} in the published Nanocodex source tree.`
        : "Browse the published Nanocodex source tree.",
      eyebrow: "SOURCE",
      title: sourcePath ? compactLabel(sourcePath, 72) : "Source code",
    };
  }
  if (pathname === "/docs" || pathname.startsWith("/docs/")) {
    const document = docsPreview[pathname as keyof typeof docsPreview];
    const fallbackTitle = pathname === "/docs" ? "Documentation" : titleFromPath(pathname);
    return {
      canonicalPath: pathname,
      description: document?.[1] ?? "Build and evaluate retained Codex agents with the Nanocodex SDK.",
      eyebrow: "DOCUMENTATION",
      title: document?.[0] ?? fallbackTitle,
    };
  }
  if (pathname === "/evals") {
    return fixed(pathname, "Evals", "Inspect retained, verifier-backed Nanocodex evaluation worksets and runs.", "EVALUATIONS");
  }
  if (pathname.startsWith("/evals/")) return evalPreview(pathname, env);
  return {
    canonicalPath: "/",
    description: "High-performance Codex SDK. Runs anywhere.",
    eyebrow: "HIGH-PERFORMANCE CODEX SDK",
    title: SITE_NAME,
  };
}

function isAgentDocumentPath(pathname: string): boolean {
  return /^\/agent\/[^/]+$/.test(pathname);
}

async function evalPreview(pathname: string, env: LinkPreviewEnv): Promise<Preview> {
  const segments = pathname.split("/").filter(Boolean);
  if (segments.length === 3 && segments[1] === "worksets") {
    const worksetId = decodeSegment(segments[2]);
    if (!worksetId) return evalFallback(pathname, "Evaluation workset");
    const row = await safeFirst<{ profile: string; task_count: number }>(env.EVALS_DB,
      `SELECT w.profile, COUNT(d.id) AS task_count FROM worksets w
       LEFT JOIN task_definitions d ON d.workset_id = w.id
       WHERE w.state = 'ready' AND w.digest = ?1 GROUP BY w.id`, worksetId);
    const title = row?.profile ? compactLabel(row.profile, 72) : compactId(worksetId, "Workset");
    return {
      canonicalPath: pathname,
      description: row
        ? `${Number(row.task_count)} retained tasks in this verifier-backed evaluation workset.`
        : "Inspect retained tasks and verifier-backed runs in this evaluation workset.",
      eyebrow: "EVALUATION WORKSET",
      title,
    };
  }
  if (segments.length === 5 && segments[1] === "worksets" && segments[3] === "tasks") {
    const worksetId = decodeSegment(segments[2]);
    const taskId = decodeSegment(segments[4]);
    if (!worksetId || !taskId) return evalFallback(pathname, "Evaluation run");
    const row = await safeFirst<{ name: string; profile: string }>(env.EVALS_DB,
      `SELECT d.name, w.profile FROM worksets w JOIN task_definitions d ON d.workset_id = w.id
       WHERE w.state = 'ready' AND w.digest = ?1 AND d.public_id = ?2`, worksetId, taskId);
    return {
      canonicalPath: pathname,
      description: row
        ? `Inspect retained ${row.profile} treatments, runs, and verifier outcomes for this task.`
        : "Inspect retained treatments, runs, and verifier outcomes for this evaluation task.",
      eyebrow: "EVALUATION RUNS",
      title: row?.name ? compactLabel(row.name, 72) : compactId(taskId, "Evaluation task"),
    };
  }
  return evalFallback(pathname, "Evals");
}

async function safeFirst<T>(db: D1Database | undefined, query: string, ...values: unknown[]): Promise<T | null> {
  if (!db) return null;
  try {
    return await db.prepare(query).bind(...values).first<T>();
  } catch {
    return null;
  }
}

function evalFallback(pathname: string, title: string): Preview {
  return {
    canonicalPath: pathname,
    description: "Inspect retained, verifier-backed Nanocodex evaluation worksets and runs.",
    eyebrow: "EVALUATIONS",
    title,
  };
}

function fixed(path: string, title: string, description: string, eyebrow = SITE_NAME.toUpperCase()): Preview {
  return { canonicalPath: path, description, eyebrow, title };
}

function metadataHtml(
  preview: Preview,
  canonicalUrl: string,
  imageUrl: string,
  deploymentSha?: string,
): string {
  const title = preview.title === SITE_NAME ? "Nanocodex — high-performance Codex SDK" : `${preview.title} · Nanocodex`;
  const alt = `${preview.title} — Nanocodex link preview`;
  const values = { alt, canonicalUrl, description: preview.description, imageUrl, title };
  const deploymentMetadata = GIT_SHA_PATTERN.test(deploymentSha ?? "")
    ? `\n    <meta name="nanocodex-deployment-sha" content="${deploymentSha}" />`
    : "";
  return `${METADATA_START}
    <title>${html(values.title)}</title>
    ${deploymentMetadata}
    <link rel="canonical" href="${html(values.canonicalUrl)}" />
    <meta name="description" content="${html(values.description)}" />
    <meta property="og:type" content="website" />
    <meta property="og:url" content="${html(values.canonicalUrl)}" />
    <meta property="og:site_name" content="${SITE_NAME}" />
    <meta property="og:title" content="${html(values.title)}" />
    <meta property="og:description" content="${html(values.description)}" />
    <meta property="og:image" content="${html(values.imageUrl)}" />
    <meta property="og:image:width" content="${IMAGE_WIDTH}" />
    <meta property="og:image:height" content="${IMAGE_HEIGHT}" />
    <meta property="og:image:type" content="image/png" />
    <meta property="og:image:alt" content="${html(values.alt)}" />
    <meta name="twitter:card" content="summary_large_image" />
    <meta name="twitter:title" content="${html(values.title)}" />
    <meta name="twitter:description" content="${html(values.description)}" />
    <meta name="twitter:image" content="${html(values.imageUrl)}" />
    <meta name="twitter:image:alt" content="${html(values.alt)}" />
    ${METADATA_END}`;
}

function injectMetadata(document: string, metadata: string): string {
  const start = document.indexOf(METADATA_START);
  const end = document.indexOf(METADATA_END);
  if (start >= 0 && end >= start) {
    return `${document.slice(0, start)}${metadata}${document.slice(end + METADATA_END.length)}`;
  }
  return document.replace("</head>", `${metadata}\n  </head>`);
}

function normalizePath(pathname: string): string {
  const path = pathname.replace(/\/+$/, "");
  return path || "/";
}

function boundedPreviewPath(value: string | null): string {
  if (!value || value.length > 1024 || !value.startsWith("/") || value.startsWith("//")) return "/";
  try {
    const url = new URL(value, "https://preview.invalid");
    return url.origin === "https://preview.invalid" ? `${url.pathname}${url.search}` : "/";
  } catch {
    return "/";
  }
}

function boundedText(value: string | null, maximum: number): string | null {
  if (!value) return null;
  const normalized = value.replace(/[\u0000-\u001f\u007f]/g, "").trim();
  return normalized && normalized.length <= maximum ? normalized : null;
}

function decodeSegment(value: string | undefined): string | null {
  if (!value || value.length > 512) return null;
  try {
    return boundedText(decodeURIComponent(value), 240);
  } catch {
    return null;
  }
}

function compactId(value: string, prefix: string): string {
  return `${prefix} ${value.length > 18 ? `${value.slice(0, 12)}…${value.slice(-6)}` : value}`;
}

function compactLabel(value: string, maximum: number): string {
  const normalized = value.replace(/\s+/g, " ").trim();
  return normalized.length <= maximum ? normalized : `${normalized.slice(0, maximum - 1).trimEnd()}…`;
}

function titleFromPath(pathname: string): string {
  const segment = pathname.split("/").filter(Boolean).at(-1) ?? "Documentation";
  const decoded = decodeSegment(segment) ?? "Documentation";
  return decoded.replace(/[-_]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function html(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;",
  })[character]!);
}

function fnv1a(value: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

const FONT: Record<string, string> = {
  " ": "00000000000000000000000000000000000", "-": "00000000000001110000000000000000000",
  ".": "00000000000000000000000000110001100", "/": "00001000100010001000100010000000000",
  ":": "00000011000110000000110001100000000", "%": "11001110100010001000101110011000000",
  "0": "01110100011001110101110011000101110", "1": "00100011000010000100001000010001110",
  "2": "01110100010000100110010001000011111", "3": "11110000010000101110000010000111110",
  "4": "00010001100101010010111110001000010", "5": "11111100001111000001000011000101110",
  "6": "00110010001000011110100011000101110", "7": "11111000010001000100010000100001000",
  "8": "01110100011000101110100011000101110", "9": "01110100011000101111000010001001100",
  "A": "01110100011000111111100011000110001", "B": "11110100011000111110100011000111110",
  "C": "01111100001000010000100001000001111", "D": "11110100011000110001100011000111110",
  "E": "11111100001000011110100001000011111", "F": "11111100001000011110100001000010000",
  "G": "01111100001000010111100011000101111", "H": "10001100011000111111100011000110001",
  "I": "01110001000010000100001000010001110", "J": "00001000010000100001100011000101110",
  "K": "10001100101010011000101001001010001", "L": "10000100001000010000100001000011111",
  "M": "10001110111010110101100011000110001", "N": "10001110011010110011100011000110001",
  "O": "01110100011000110001100011000101110", "P": "11110100011000111110100001000010000",
  "Q": "01110100011000110001101011001001101", "R": "11110100011000111110101001001010001",
  "S": "01111100001000001110000010000111110", "T": "11111001000010000100001000010000100",
  "U": "10001100011000110001100011000101110", "V": "10001100011000110001100010101000100",
  "W": "10001100011000110101101011010101010", "X": "10001100010101000100010101000110001",
  "Y": "10001100010101000100001000010000100", "Z": "11111000010001000100010001000011111",
};

async function renderPng(preview: Preview): Promise<Uint8Array> {
  const pixels = new Uint8Array(IMAGE_WIDTH * IMAGE_HEIGHT * 3);
  pixels.fill(22);
  fillRect(pixels, 72, 64, 8, 502, 255);
  drawText(pixels, "NANOCODEX", 108, 72, 4, 170);
  drawText(pixels, ascii(preview.eyebrow), 108, 138, 4, 255);
  const lines = wrap(ascii(preview.title), 25, 3);
  lines.forEach((line, index) => drawText(pixels, line, 108, 226 + index * 78, 9, 255));
  const detailSource = preview.canonicalPath === "/"
    ? "82.2% NANOCODEX  /  79.6% CODEX  /  890/890 RUNS"
    : ascii(preview.description);
  const detail = detailSource.length <= 54 ? detailSource : `${detailSource.slice(0, 51).trimEnd()}...`;
  drawText(pixels, detail, 108, 532, 3, 190);
  return encodePng(pixels);
}

function ascii(value: string): string {
  return value.normalize("NFKD").replace(/[^\x20-\x7e]/g, " ").toUpperCase().replace(/\s+/g, " ").trim();
}

function wrap(value: string, width: number, maximumLines: number): string[] {
  const words = value.split(" ");
  const lines: string[] = [];
  for (const word of words) {
    const chunks = word.match(new RegExp(`.{1,${width}}`, "g")) ?? [];
    for (const chunk of chunks) {
      const current = lines.at(-1);
      if (current && current.length + chunk.length + 1 <= width) lines[lines.length - 1] = `${current} ${chunk}`;
      else lines.push(chunk);
    }
  }
  if (lines.length > maximumLines) {
    lines.length = maximumLines;
    lines[maximumLines - 1] = `${lines[maximumLines - 1]!.slice(0, width - 3)}...`;
  }
  return lines.length ? lines : [SITE_NAME.toUpperCase()];
}

function drawText(pixels: Uint8Array, value: string, x: number, y: number, scale: number, color: number): void {
  let cursor = x;
  for (const character of value) {
    const glyph = FONT[character] ?? FONT[" "]!;
    for (let row = 0; row < 7; row += 1) for (let column = 0; column < 5; column += 1) {
      if (glyph[row * 5 + column] === "1") fillRect(pixels, cursor + column * scale, y + row * scale, scale, scale, color);
    }
    cursor += 6 * scale;
    if (cursor >= IMAGE_WIDTH - 72) break;
  }
}

function fillRect(pixels: Uint8Array, x: number, y: number, width: number, height: number, color: number): void {
  for (let row = Math.max(0, y); row < Math.min(IMAGE_HEIGHT, y + height); row += 1) {
    for (let column = Math.max(0, x); column < Math.min(IMAGE_WIDTH, x + width); column += 1) {
      const offset = (row * IMAGE_WIDTH + column) * 3;
      pixels[offset] = color; pixels[offset + 1] = color; pixels[offset + 2] = color;
    }
  }
}

async function encodePng(pixels: Uint8Array): Promise<Uint8Array> {
  const scanlines = new Uint8Array(IMAGE_HEIGHT * (IMAGE_WIDTH * 3 + 1));
  for (let row = 0; row < IMAGE_HEIGHT; row += 1) {
    const target = row * (IMAGE_WIDTH * 3 + 1);
    scanlines.set(pixels.subarray(row * IMAGE_WIDTH * 3, (row + 1) * IMAGE_WIDTH * 3), target + 1);
  }
  const stream = new Blob([scanlines]).stream().pipeThrough(new CompressionStream("deflate"));
  const compressed = new Uint8Array(await new Response(stream).arrayBuffer());
  const header = new Uint8Array(13);
  new DataView(header.buffer).setUint32(0, IMAGE_WIDTH);
  new DataView(header.buffer).setUint32(4, IMAGE_HEIGHT);
  header.set([8, 2, 0, 0, 0], 8);
  return concat(new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header), chunk("IDAT", compressed), chunk("IEND", new Uint8Array()));
}

function chunk(type: string, data: Uint8Array): Uint8Array {
  const typeBytes = new TextEncoder().encode(type);
  const output = new Uint8Array(data.length + 12);
  new DataView(output.buffer).setUint32(0, data.length);
  output.set(typeBytes, 4); output.set(data, 8);
  new DataView(output.buffer).setUint32(output.length - 4, crc32(concat(typeBytes, data)));
  return output;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const output = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of parts) { output.set(part, offset); offset += part.length; }
  return output;
}

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}
