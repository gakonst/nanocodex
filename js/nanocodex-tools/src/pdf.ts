import type { Workspace } from "../tools/types.mjs";

type Context = Readonly<{ cwd?: unknown; signal?: AbortSignal }>;
type TextItem = { str: string; transform: number[]; width: number; height: number; hasEOL?: boolean };
type Options = { input: string; output: string; first: number; last?: number; layout: boolean; raw: boolean; pageBreaks: boolean };

const HELP = `pdftotext (PDF.js, local files; no sandbox required)
Usage: pdftotext [options] input.pdf [output.txt|-]
  -f N, -l N    First and last page (1-based, inclusive)
  -layout       Approximate physical layout with spaces
  -raw          Preserve PDF content-stream order
  -nopgbrk      Omit form-feed page separators
  -enc UTF-8    UTF-8 output (the only supported encoding)
  -h, --help    Show this help
Output defaults to input.txt. Use - for stdout. Input must be a local file.
Text extraction only: scanned pages need OCR. Layout is approximate, not
byte-for-byte Poppler output. Unsupported flags are rejected.
`;

/** PDF text extraction in the existing JS runtime, with no host process or fetch. */
export function createPdfTextCommand(filesystem: () => Workspace) {
  return {
    name: "pdftotext",
    trusted: true,
    async execute(args: string[], context: Context = {}) {
      let document: Awaited<ReturnType<typeof import("unpdf")["getDocumentProxy"]>> | undefined;
      try {
        context.signal?.throwIfAborted();
        if (args.length === 1 && ["-h", "-help", "--help"].includes(args[0]!)) {
          return { stdout: HELP, stderr: "", exitCode: 0 };
        }
        const options = parse(args);
        const workspace = filesystem();
        const cwd = typeof context.cwd === "string" ? context.cwd : workspace.root;
        const input = resolve(workspace, cwd, options.input);
        const output = options.output === "-" ? undefined : resolve(workspace, cwd, options.output);
        if (input === output) throw new Error("input and output must be different files");
        if (output) {
          const parent = output.slice(0, output.lastIndexOf("/")) || "/";
          await workspace.list(parent);
          const existing = (await workspace.list(parent)).find(entry => entry.path === output);
          if (existing && existing.kind !== "file") throw new Error("output must be a regular file");
        }
        const data = await workspace.readFile(input);
        context.signal?.throwIfAborted();
        const { getDocumentProxy } = await import("unpdf");
        document = await getDocumentProxy(data.slice(), {
          isEvalSupported: false,
          useSystemFonts: false,
          disableFontFace: true,
          useWorkerFetch: false,
          isOffscreenCanvasSupported: false,
          verbosity: 0,
        });
        const last = Math.min(options.last ?? document.numPages, document.numPages);
        if (options.first > last) throw new Error("page range is outside the document");
        const pages: string[] = [];
        for (let number = options.first; number <= last; number++) {
          context.signal?.throwIfAborted();
          const page = await document.getPage(number);
          try {
            const content = await page.getTextContent();
            const items = content.items.filter(item => "str" in item) as TextItem[];
            pages.push(formatPage(items, options));
          } finally { page.cleanup(); }
        }
        const text = pages.map(page => page + (options.pageBreaks ? "\f" : "")).join("");
        context.signal?.throwIfAborted();
        if (output) await workspace.writeFile(output, text);
        return { stdout: output ? "" : text, stderr: "", exitCode: 0 };
      } catch (error) {
        return { stdout: "", stderr: `pdftotext: ${error instanceof Error ? error.message : String(error)}\n`,
          exitCode: context.signal?.aborted ? 130 : 1 };
      } finally { await document?.destroy(); }
    },
  };
}

function parse(args: string[]): Options {
  const options: Options = { input: "", output: "", first: 1, layout: false, raw: false, pageBreaks: true };
  const files: string[] = [];
  let positional = false;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!;
    if (arg.includes("\0")) throw new Error("NUL in argument");
    if (!positional && arg === "--") { positional = true; continue; }
    if (!positional && ["-f", "-l"].includes(arg)) {
      const value = args[++index];
      if (!value || !/^[1-9][0-9]*$/.test(value) || !Number.isSafeInteger(Number(value))) throw new Error(`${arg} requires a positive page number`);
      if (arg === "-f") options.first = Number(value); else options.last = Number(value);
    } else if (!positional && arg === "-layout") options.layout = true;
    else if (!positional && arg === "-raw") options.raw = true;
    else if (!positional && arg === "-nopgbrk") options.pageBreaks = false;
    else if (!positional && arg === "-enc") {
      if (args[++index]?.toUpperCase() !== "UTF-8") throw new Error("only -enc UTF-8 is supported");
    } else if (!positional && arg.startsWith("-") && arg !== "-") throw new Error(`unsupported option '${arg}' (see --help)`);
    else files.push(arg);
  }
  if (!files.length || files.length > 2 || files[0] === "-") throw new Error("usage: pdftotext [options] input.pdf [output.txt|-]");
  if (options.layout && options.raw) throw new Error("-layout and -raw cannot be combined");
  if (options.last !== undefined && options.last < options.first) throw new Error("last page precedes first page");
  options.input = files[0]!;
  options.output = files[1] ?? (/\.pdf$/i.test(options.input) ? options.input.replace(/\.pdf$/i, ".txt") : `${options.input}.txt`);
  return options;
}

function resolve(workspace: Workspace, cwd: string, path: string): string {
  if (!path || /^[a-z][a-z0-9+.-]*:/i.test(path)) throw new Error("only local workspace files are supported");
  const components: string[] = [];
  for (const part of (path.startsWith("/") ? path : `${cwd}/${path}`).split("/")) {
    if (part === "..") components.pop();
    else if (part && part !== ".") components.push(part);
  }
  const absolute = `/${components.join("/")}`;
  if (!absolute.startsWith(`${workspace.root}/`)) throw new Error("path is outside the workspace");
  return absolute;
}

function formatPage(items: TextItem[], options: Options): string {
  if (options.raw) {
    return items.map(item => item.str + (item.hasEOL ? "\n" : " ")).join("").trimEnd() + "\n";
  }
  const lines: { y: number; height: number; items: TextItem[] }[] = [];
  for (const item of items) {
    if (!item.str) continue;
    const y = item.transform[5] ?? 0;
    const height = Math.abs(item.height) || Math.hypot(item.transform[2] ?? 0, item.transform[3] ?? 0) || 12;
    let line = lines.find(line => Math.abs(line.y - y) <= Math.min(line.height, height) * 0.35);
    if (!line) { line = { y, height, items: [] }; lines.push(line); }
    line.items.push(item);
  }
  lines.sort((a, b) => b.y - a.y);
  const widths = items.filter(item => item.str.trim() && item.width > 0).map(item => item.width / item.str.length).sort((a, b) => a - b);
  const cell = Math.max(1, widths[Math.floor(widths.length / 2)] ?? 6);
  const left = Math.min(...items.filter(item => item.str).map(item => item.transform[4] ?? 0));
  return lines.map(line => {
    line.items.sort((a, b) => (a.transform[4] ?? 0) - (b.transform[4] ?? 0));
    let text = "", right = left;
    for (const item of line.items) {
      const x = item.transform[4] ?? 0;
      const spaces = options.layout
        ? Math.max(0, Math.round((x - left) / cell) - text.length)
        : text && x - right > cell * 0.2 && !text.endsWith(" ") && !item.str.startsWith(" ") ? 1 : 0;
      // Bad coordinates must not allocate unbounded padding.
      text += " ".repeat(Math.min(spaces, 10_000)) + item.str;
      right = x + item.width;
    }
    return text.trimEnd();
  }).join("\n") + "\n";
}
