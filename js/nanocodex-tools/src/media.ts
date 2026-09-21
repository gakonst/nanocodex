import type { Workspace, WorkspaceEntry } from "../tools/types.mjs";

export type MediaFile = Readonly<{ path: string; data: Uint8Array }>;
export type MediaRequest = Readonly<{
  command: "ffmpeg" | "ffprobe";
  args: readonly string[];
  files: readonly MediaFile[];
}>;
export type MediaResult = Readonly<{
  stdout: string;
  stderr: string;
  exitCode: number;
  files: readonly MediaFile[];
}>;
export type MediaCommandOptions = Readonly<{
  filesystem: () => Workspace;
  /** Execute in an isolated, ephemeral memory filesystem without host files or network. */
  execute: (request: MediaRequest, signal?: AbortSignal) => Promise<MediaResult>;
}>;

type CommandContext = Readonly<{ cwd?: unknown; signal?: AbortSignal }>;
const flags = new Set(["-hide_banner", "-vn", "-an", "-sn", "-dn", "-y", "-n"]);
const probeFlags = new Set(["-hide_banner", "-show_format", "-show_streams", "-show_error", "-count_frames", "-count_packets"]);
const informational = new Set(["-version", "-formats", "-codecs", "-decoders", "-encoders", "-filters"]);
const outputExtensions = new Set(["jpg", "jpeg", "wav"]);
const formats = new Set(["mov", "mp4", "matroska", "webm", "avi", "wav", "mp3", "flac", "ogg", "aac", "gif", "image2", "mjpeg", "rawvideo", "s16le", "s24le", "f32le"]);
const scalars: Record<string, RegExp> = {
  "-v": /^(quiet|panic|fatal|error|warning|info|verbose|debug|trace|[0-9]+)$/,
  "-loglevel": /^(quiet|panic|fatal|error|warning|info|verbose|debug|trace|[0-9]+)$/,
  "-ss": /^\d+(?:\.\d+)?(?::\d+(?:\.\d+)?){0,2}$/,
  "-t": /^\d+(?:\.\d+)?(?::\d+(?:\.\d+)?){0,2}$/,
  "-to": /^\d+(?:\.\d+)?(?::\d+(?:\.\d+)?){0,2}$/,
  "-c": /^[A-Za-z0-9_]+$/,
  "-codec": /^[A-Za-z0-9_]+$/,
  "-b": /^\d+(?:\.\d+)?[kKmM]?$/,
  "-q": /^\d+(?:\.\d+)?$/,
  "-qscale": /^\d+(?:\.\d+)?$/,
  "-crf": /^\d+(?:\.\d+)?$/,
  "-frames": /^\d+$/,
  "-ac": /^\d+$/,
  "-ar": /^\d+$/,
  "-r": /^\d+(?:\.\d+)?(?:\/\d+)?$/,
  "-s": /^\d+x\d+$/,
  "-pix_fmt": /^[A-Za-z0-9_]+$/,
  "-sample_fmt": /^[A-Za-z0-9_]+$/,
  "-preset": /^[A-Za-z0-9_]+$/,
  "-tune": /^[A-Za-z0-9_,]+$/,
  "-profile": /^[A-Za-z0-9_]+$/,
  "-level": /^\d+(?:\.\d+)?$/,
  "-threads": /^\d+$/,
  "-filter_threads": /^\d+$/,
  "-map": /^0(?::[vasd])?(?::\d+)?\??$/,
  "-map_metadata": /^-1$/,
  "-map_chapters": /^-1$/,
  "-movflags": /^\+?(?:faststart|use_metadata_tags)(?:\+(?:faststart|use_metadata_tags))*$/,
};
const probeScalars: Record<string, RegExp> = {
  "-v": scalars["-v"]!,
  "-loglevel": scalars["-loglevel"]!,
  "-show_entries": /^[A-Za-z0-9_,=:]+$/,
  "-select_streams": /^(?:[vasd](?::\d+)?|\d+)$/,
  "-of": /^(json|default|compact|csv|flat|ini|xml)$/,
  "-print_format": /^(json|default|compact|csv|flat|ini|xml)$/,
};

/** Local-file FFmpeg commands; the caller owns the isolated WASM executor. */
export function createMediaCommands(options: MediaCommandOptions) {
  return (["ffmpeg", "ffprobe"] as const).map((command) => ({
    name: command,
    trusted: true,
    async execute(args: string[], context: CommandContext = {}) {
      try {
        context.signal?.throwIfAborted();
        validateArguments(args);
        if (args.length === 0 || (args.length === 1 && ["-h", "-help", "--help", "help"].includes(args[0]!))) {
          return { stdout: help(command), stderr: "", exitCode: 0 };
        }
        const parsed = parse(command, args);
        const files: MediaFile[] = [];
        let workspace: Workspace | undefined;
        let output: string | undefined;
        if (parsed.input !== undefined) {
          workspace = options.filesystem();
          const cwd = typeof context.cwd === "string" ? context.cwd : workspace.root;
          const input = resolve(workspace, cwd, parsed.input);
          if (parsed.output !== undefined) {
            output = resolve(workspace, cwd, parsed.output);
            if (input === output) throw new Error("input and output must be different files");
            await checkOutput(workspace, output, parsed.overwrite);
          }
          const entry = await stat(workspace, input);
          if (!entry || entry.kind !== "file") throw new Error("input must be an existing regular file");
          context.signal?.throwIfAborted();
          const data = await workspace.readFile(input);
          files.push({ path: parsed.inputPath!, data });
        }
        context.signal?.throwIfAborted();
        const result = await options.execute({ command, args: parsed.args, files }, context.signal);
        context.signal?.throwIfAborted();
        if (!result || !Number.isSafeInteger(result.exitCode) || result.exitCode < 0
          || typeof result.stdout !== "string" || typeof result.stderr !== "string" || !Array.isArray(result.files)) {
          throw new Error("invalid media executor result");
        }
        const response = { stdout: result.stdout, stderr: result.stderr, exitCode: result.exitCode };
        if (result.exitCode !== 0) return response;
        if (output !== undefined && workspace) {
          if (result.files.length !== 1 || result.files[0]?.path !== parsed.outputPath
            || !(result.files[0]?.data instanceof Uint8Array)) throw new Error("executor did not return the requested output file");
          const data = result.files[0].data;
          if (data.byteLength === 0) throw new Error("executor returned an empty output file");
          await checkOutput(workspace, output, parsed.overwrite);
          context.signal?.throwIfAborted();
          await workspace.writeFile(output, data);
        } else if (result.files.length) {
          throw new Error("executor returned unexpected files");
        }
        return response;
      } catch (error) {
        return { stdout: "", stderr: `${command}: ${error instanceof Error ? error.message : String(error)}\n`, exitCode: context.signal?.aborted ? 130 : 1 };
      }
    },
  }));
}

function validateArguments(args: string[]) {
  if (!Array.isArray(args)) throw new Error("arguments must be an array");
  for (const arg of args) {
    if (typeof arg !== "string" || !arg || /[\x00-\x1f\x7f]/.test(arg)) throw new Error("arguments must be nonempty text without control characters");
  }
}

function parse(command: "ffmpeg" | "ffprobe", args: string[]) {
  if (args.length === 1 && informational.has(args[0]!)) return { args: [...args], overwrite: false };
  // Durable overwrite policy is checked separately; ephemeral output may be replaced.
  const rewritten: string[] = command === "ffmpeg" ? ["-nostdin", "-y"] : [];
  let input: string | undefined;
  let output: string | undefined;
  let inputPath: string | undefined;
  let outputPath: string | undefined;
  let overwrite = false;
  let noOverwrite = false;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index]!;
    if (command === "ffmpeg" && arg === "-i") {
      if (input !== undefined) throw new Error("exactly one local input is supported");
      input = args[++index];
      inputPath = memoryPath(input, "input");
      rewritten.push("-protocol_whitelist", "file", "-i", inputPath);
    } else if ((command === "ffmpeg" ? flags : probeFlags).has(arg)) {
      overwrite ||= arg === "-y";
      noOverwrite ||= arg === "-n";
      if (arg !== "-y" && arg !== "-n") rewritten.push(arg);
    } else if (arg.startsWith("-")) {
      const scalar = command === "ffprobe" ? arg : arg.replace(/:[va](?::\d+)?$/, "");
      const pattern = (command === "ffprobe" ? probeScalars : scalars)[scalar];
      const value = args[++index];
      if (value === undefined) throw new Error(`missing value for ${arg}`);
      if (command === "ffmpeg" && (arg === "-vf" || arg === "-af")) validateFilter(value, arg);
      else if (command === "ffmpeg" && arg === "-f") {
        if (!formats.has(value)) throw new Error("unsupported media format");
      } else if (!pattern || !pattern.test(value)) throw new Error(`unsupported option or value: ${arg}`);
      rewritten.push(arg, value);
    } else if (command === "ffprobe") {
      if (input !== undefined) throw new Error("exactly one local input is supported");
      input = arg;
      inputPath = memoryPath(input, "input");
      rewritten.push("-protocol_whitelist", "file", inputPath);
    } else {
      if (output !== undefined) throw new Error("exactly one local output is supported");
      output = arg;
      outputPath = memoryPath(output, "output");
      rewritten.push(outputPath);
      if (index !== args.length - 1) throw new Error("output must be the final argument");
    }
  }
  if (overwrite && noOverwrite) throw new Error("-y and -n cannot be combined");
  if (input === undefined || (command === "ffmpeg" && output === undefined)) throw new Error("one input and, for ffmpeg, one output are required");
  return { args: rewritten, input, output, inputPath, outputPath, overwrite };
}

// Only filters with scalar parameters and no secondary file, script, or URL inputs.
function validateFilter(value: string, option: string) {
  const video: Record<string, RegExp> = {
    fps: /^\d+(?:\.\d+)?(?:\/\d+)?$/,
    scale: /^-?\d+:-?\d+(?::flags=(?:bilinear|bicubic|lanczos|neighbor))?$/,
    tile: /^\d+x\d+$/,
    crop: /^\d+:\d+(?::\d+:\d+)?$/,
    transpose: /^[0-3]$/,
    hflip: /^$/,
    vflip: /^$/,
    format: /^[A-Za-z0-9_]+$/,
  };
  const audio: Record<string, RegExp> = {
    aresample: /^\d+$/,
  };
  const filters = value.split(",");
  for (const filter of filters) {
    const separator = filter.indexOf("=");
    const name = separator < 0 ? filter : filter.slice(0, separator);
    const parameters = separator < 0 ? "" : filter.slice(separator + 1);
    if (!(option === "-vf" ? video : audio)[name]?.test(parameters)) throw new Error(`unsupported ${option} filter or parameters`);
  }
}

function memoryPath(path: string | undefined, name: string) {
  if (!path || path.startsWith("-") || /[:\\%?#]/.test(path)) throw new Error("only local file paths are supported");
  const extension = /\.([A-Za-z0-9]+)$/.exec(path)?.[1]?.toLowerCase();
  if (!extension) throw new Error("media paths require a filename extension");
  if (name === "output" && !outputExtensions.has(extension)) throw new Error("unsupported output extension; use JPEG (.jpg/.jpeg) or PCM audio (.wav)");
  return `/${name}.${extension}`;
}

function resolve(workspace: Workspace, cwd: string, path: string) {
  const root = workspace.root.replace(/\/$/, "");
  const segments: string[] = [];
  for (const part of (path.startsWith("/") ? path : `${cwd}/${path}`).split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") segments.pop();
    else segments.push(part);
  }
  const resolved = `/${segments.join("/")}`;
  if (resolved !== root && !resolved.startsWith(`${root}/`)) throw new Error(`path escapes ${root}`);
  return resolved;
}

async function stat(workspace: Workspace, path: string): Promise<WorkspaceEntry | undefined> {
  if (path === workspace.root) return { kind: "directory", path };
  const separator = path.lastIndexOf("/");
  const parent = path.slice(0, separator);
  const name = path.slice(separator + 1);
  // Workspace storage rejects symbolic links; never bypass it with a host filesystem.
  return (await workspace.list(parent)).find((entry) => entry.path === path || entry.path === name);
}

async function checkOutput(workspace: Workspace, path: string, overwrite: boolean) {
  const parent = path.slice(0, path.lastIndexOf("/"));
  const directory = await stat(workspace, parent);
  if (!directory || directory.kind !== "directory") throw new Error("output parent must be an existing directory");
  const existing = await stat(workspace, path);
  if (existing && existing.kind !== "file") throw new Error("output must be a regular file");
  if (existing && !overwrite) throw new Error("output already exists; use -y to overwrite");
}

function help(command: string) {
  return `${command} (isolated WASM media command)\n`
    + (command === "ffmpeg" ? "Usage: ffmpeg [options] -i INPUT [options] OUTPUT\n" : "Usage: ffprobe [options] INPUT\n")
    + "One local input, and one ffmpeg output. Output parent must exist. Cloudflare runtime limits apply.\n"
    + "Use -y to replace an output; -n preserves it. URLs, scripts and extra file inputs are unsupported.\n"
    + "Information: -version, -formats, -codecs. Common options: -v/-loglevel, -ss, -t, -c:v/-c:a, -ac, -ar.\n"
    + "Outputs: JPEG (.jpg/.jpeg), PCM WAV (.wav). Video filters: fps, scale, tile, crop, transpose, hflip, vflip, format.\n"
    + "ffprobe: -show_entries, -show_format, -show_streams, -of json.\n";
}
