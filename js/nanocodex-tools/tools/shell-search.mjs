// Host-owned search/text commands. Literal-context searches avoid synchronous
// RE2; other regex/program syntax stays upstream behind source/read admission.
const CHUNK = 64 * 1024;
const FALLBACK_WORK = 2_000_000;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function createSearchCommands({ Bash }) {
  const names = ["grep", "fgrep", "egrep", "rg", "sed", "awk"];
  const originals = new Bash({ commands: names }).commands;
  return names.map((name) => ({
    name,
    trusted: true,
    async execute(args, ctx) {
      try {
        const command = originals.get(name);
        if (name === "sed" || name === "awk") return await boundedCommand(command, name, args, ctx, programArguments(name, args));
        const parsed = parseArgs(name, args);
        const template = parsed && parseTemplate(parsed.pattern, parsed.mode);
        let directoryInput = false;
        if (template) for (const path of parsed.files) {
          if (path === "-") continue;
          try { if ((await ctx.fs.stat(ctx.fs.resolvePath(ctx.cwd, path))).isDirectory) directoryInput = true; } catch {}
        }
        const fallback = () => boundedCommand(command, name, args, ctx, searchArguments(name, args));
        // Explicit -i and upstream smart-case share cooperative ASCII folding.
        // Smart-case checks ASCII capitals in the original pattern.
        // Fold ASCII literals cooperatively, but leave Unicode case folding and
        // upstream's Unicode prefilter semantics with the original command.
        const smartCase = name === "rg" && template && (parsed.ignoreCase || !/[A-Z]/.test(parsed.pattern));
        const asciiCase = smartCase && /^[\x00-\x7f]*$/.test(template.literal);
        if (template && (!smartCase || asciiCase) && !directoryInput && (parsed.files.length > 0 || ctx.stdin.length > 0 || name !== "rg")) {
          return await search(name, parsed, asciiCase ? { ...template, literal: template.literal.toLowerCase() } : template, ctx, asciiCase ? fallback : undefined);
        }
        return await fallback();
      } catch (error) {
        if (error?.fatalSearchAdmission) throw error;
        if (error?.shellLimit) return refusal(ctx, name, error.message.replace(`${name}: `, ""));
        throw error;
      }
    },
  }));
}

// Load/admit every source before the original can compile. The final budget
// includes stdin AND source files, plus every subsequent data read.
async function boundedCommand(command, name, args, ctx, plan) {
  if (plan.uncertain) return refusal(ctx, name, "uncertain regex/script admission; use a native Hand");
  const paths = plan.paths ?? new Set();
  const cache = new Map(), cacheSizes = new Map(), cacheReads = new Map(), sourcePaths = new Set(), programs = [...plan.sources ?? []];
  let retained = ctx.stdin.length;
  let sourceBytes = programs.reduce((sum, text) => sum + text.length, 0);
  if (retained > ctx.limits.maxInputBytes) return refusal(ctx, name, "input admission limit exceeded; use a native Hand");
  for (const operand of paths) {
    ctx.signal?.throwIfAborted(); ctx.executionScope?.throwIfAborted();
    if (operand === "-") {
      if (ctx.stdin.length > 8192 - sourceBytes) return refusal(ctx, name, "synchronous regex/script-file compilation admission; use a native Hand");
      programs.push(decodeStdin(ctx.stdin)); sourceBytes += ctx.stdin.length;
      continue;
    }
    const path = ctx.fs.resolvePath(ctx.cwd, operand);
    sourcePaths.add(path);
    let stat;
    try { stat = await ctx.fs.stat(path); } catch { continue; } // Upstream owns missing-file diagnostics.
    if (stat.isDirectory) continue;
    if (stat.size > Math.min(8192 - sourceBytes, ctx.limits.maxInputBytes - retained)) return refusal(ctx, name, "synchronous regex/script-file compilation admission; use a native Hand");
    const text = await ctx.fs.readFile(path);
    if (text.length > 8192 - sourceBytes) return refusal(ctx, name, "synchronous regex/script-file compilation admission; use a native Hand");
    sourceBytes += stat.size; retained += stat.size;
    cache.set(path, text); cacheSizes.set(path, stat.size); programs.push(text);
  }
  if (sourceBytes > 8192) return refusal(ctx, name, "synchronous regex/script compilation admission; use a native Hand");
  const policy = name === "sed" || name === "awk" ? programPolicy(name, programs, plan) : {};
  if (policy.uncertain) return refusal(ctx, name, "uncertain dynamic regex admission; use a native Hand");
  // Recognized search and stream sources are isolated from data operands.
  // A filename such as report{5000}.txt is not regex source.
  const cost = policy.safe ? 1 : name === "sed" || name === "awk"
    ? Math.max(sourceBytes, fallbackCost(policy.costSources ?? programs)) : Math.max(fallbackCost(plan.costSources ?? []), fallbackCost(programs));
  if (cost >= FALLBACK_WORK) return refusal(ctx, name, "synchronous regex compilation/work admission; simplify the pattern or use a native Hand");
  const cap = Math.min(ctx.limits.maxInputBytes, Math.max(1, Math.floor((policy.safe ? ctx.limits.maxInputBytes : FALLBACK_WORK) / cost)));
  const reason = () => `synchronous regex input/work admission (${cap} bytes); use a native Hand`;
  if (retained > cap) return refusal(ctx, name, reason());
  const fs = new Proxy(ctx.fs, {
    get(target, key) {
      const method = Reflect.get(target, key, target);
      if (typeof method !== "function") return method;
      if (!["readFile", "readFileBuffer", "readFileBytes"].includes(key)) return method.bind(target);
      return async (path, ...options) => {
        ctx.signal?.throwIfAborted(); ctx.executionScope?.throwIfAborted();
        if (cache.has(path)) {
          const supplied = cacheReads.get(path) ?? 0;
          cacheReads.set(path, supplied + 1);
          // The first source read was precharged. Later reads may also name
          // this file as data; charge them even though storage is cached.
          if (supplied > 0) {
            const size = cacheSizes.get(path);
            if (size > cap - retained) {
              limit(ctx, name, reason());
            }
            retained += size;
          }
          const text = cache.get(path);
          if (key === "readFile") return text;
          const bytes = encoder.encode(text);
          if (key === "readFileBuffer") return bytes;
          let encoded = ""; for (const byte of bytes) encoded += String.fromCharCode(byte);
          return encoded;
        }
        const stat = await target.stat(path);
        if (sourcePaths.has(path)) {
          const changed = "unadmitted regex/script source changed; use a native Hand";
          limit(ctx, name, changed);
        }
        if (stat.size > cap - retained) {
          // Fatal accounting prevents upstream catches from masking admission.
          limit(ctx, name, reason());
        }
        retained += stat.size;
        return method.call(target, path, ...options);
      };
    },
  });
  await checkpoint(ctx);
  return await command.execute(args, { ...ctx, fs });
}

function parseArgs(name, args) {
  const value = { mode: name === "fgrep" ? "fixed" : name === "egrep" || name === "rg" ? "extended" : "basic", files: [], pattern: undefined, ignoreCase: false, only: false, number: false, count: false, filesWith: false, filesWithout: false, quiet: false, invert: false, filename: undefined, max: 0 };
  let options = true;
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (options && arg === "--") { if (name === "rg") return; options = false; continue; }
    if (options && arg.startsWith("-") && arg !== "-") {
      if ((arg === "-e" || arg === "--regexp") && index + 1 < args.length && value.pattern === undefined) { value.pattern = args[++index]; continue; }
      if (arg === "-m" || arg === "--max-count") { if (!/^\d+$/.test(args[index + 1] ?? "")) return; value.max = Number(args[++index]); continue; }
      const max = arg.match(/^(?:-m|--max-count=)(\d+)$/);
      if (max) { value.max = Number(max[1]); continue; }
      const long = { "--ignore-case": "i", "--only-matching": "o", "--line-number": "n", "--count": "c", "--files-with-matches": "l", "--files-without-match": "L", "--quiet": "q", "--silent": "q", "--no-filename": "h", "--with-filename": "H", "--invert-match": "v", "--fixed-strings": "F", "--extended-regexp": "E" };
      const flags = arg.startsWith("--") ? long[arg] : arg.slice(1);
      if (!flags) return;
      for (const flag of flags) {
        if (flag === "i") { if (name !== "rg") return; value.ignoreCase = true; }
        else if (flag === "o") value.only = true;
        else if (flag === "n") value.number = true;
        else if (flag === "c") value.count = true;
        else if (flag === "l") value.filesWith = true;
        else if (flag === "L") value.filesWithout = true;
        else if (flag === "q") value.quiet = true;
        else if (flag === "v") value.invert = true;
        else if (flag === "h") { if (name === "rg" && !arg.startsWith("--")) return; value.filename = false; }
        else if (flag === "H") { if (name !== "rg") return; value.filename = true; }
        else if (flag === "F") value.mode = "fixed";
        else if (flag === "E") { if (name === "rg") return; if (value.mode !== "fixed") value.mode = "extended"; }
        else return;
      }
    } else if (value.pattern === undefined) value.pattern = arg;
    else value.files.push(arg);
  }
  if (value.pattern === undefined || !Number.isSafeInteger(value.max)) return;
  // rg -c/-m/-L/-h have different contracts; delegate rather than approximate.
  if (name === "rg" && (value.count || value.max || value.filesWithout || args.includes("-h"))) return;
  return value;
}

function parseTemplate(pattern, mode) {
  if (!pattern || pattern.includes("\n") || pattern.length > 4096) return;
  if (mode === "fixed") return { literal: pattern, before: 0, after: 0 };
  const quantifier = mode === "basic" ? /^\.\\\{0,(\d+)\\\}/ : /^\.\{0,(\d+)\}/;
  const suffix = mode === "basic" ? /\.\\\{0,(\d+)\\\}$/ : /\.\{0,(\d+)\}$/;
  let before = 0, after = 0;
  const start = pattern.match(quantifier);
  if (start) { before = Number(start[1]); pattern = pattern.slice(start[0].length); }
  const end = pattern.match(suffix);
  if (end) { after = Number(end[1]); pattern = pattern.slice(0, -end[0].length); }
  if (!Number.isSafeInteger(before) || !Number.isSafeInteger(after) || before > 1_000_000 || after > 1_000_000) return;
  let literal = "";
  const meta = mode === "basic" ? /[.*^$[\]\\]/ : /[.*+?^${}()|[\]\\]/;
  for (let i = 0; i < pattern.length; i++) {
    const char = pattern[i];
    if (char === "\\") {
      const next = pattern[++i];
      if (next === undefined || !/[.*+?^${}()|[\]\\/\-]/.test(next)) return;
      // Basic regular expression escaped grouping/quantifiers are operators, not literals.
      if (mode === "basic" && /[(){}|]/.test(next)) return;
      literal += next;
    } else {
      if (meta.test(char)) return;
      literal += char;
    }
  }
  if (!literal || /[\uD800-\uDFFF]/u.test(literal)) return;
  return { literal, before, after };
}

async function search(name, parsed, template, ctx, unicodeFallback) {
  let files = parsed.files.length ? parsed.files : ["-"];
  const diagnostic = name === "fgrep" || name === "egrep" ? "grep" : name;
  if (name === "rg") {
    const existing = [];
    for (const file of files) {
      if (file === "-") { existing.push(file); continue; }
      try { const stat = await ctx.fs.stat(ctx.fs.resolvePath(ctx.cwd, file)); if (!stat.isDirectory) existing.push(file); } catch {}
    }
    files = [...new Set(existing)].sort();
  }
  const showName = parsed.filename ?? files.length > 1;
  const showNumber = parsed.number || name === "rg" && files.length > 1 && !parsed.only;
  const output = [];
  let bytes = 0, errors = "", any = false, stdinUsed = false;
  const append = (text) => {
    if (text.length > Math.min(ctx.limits.maxOutputSize, ctx.limits.maxStringLength) - bytes) limit(ctx, name, "output size limit exceeded");
    bytes += encoder.encode(text).byteLength;
    if (bytes > Math.min(ctx.limits.maxOutputSize, ctx.limits.maxStringLength)) limit(ctx, name, "output size limit exceeded");
    output.push(text);
  };
  for (const file of files) {
    let text;
    try {
      if (file === "-") {
        text = stdinUsed ? "" : decodeStdin(ctx.stdin); stdinUsed = true;
      } else {
        const path = ctx.fs.resolvePath(ctx.cwd, file);
        const stat = await ctx.fs.stat(path);
        if (stat.isDirectory) { errors += `${diagnostic}: ${file}: Is a directory\n`; continue; }
        if (stat.size > ctx.limits.maxInputBytes) limit(ctx, name, "input size limit exceeded");
        text = await ctx.fs.readFile(path);
      }
    } catch (error) {
      if (error?.shellLimit || error?.name === "AbortError") throw error;
      errors += `${diagnostic}: ${file}: No such file or directory\n`; continue;
    }
    if (name === "rg" && text.includes("\0")) continue;
    // Do not lowercase an unbounded string or change Unicode match offsets.
    // Returning the original command also preserves its whole-line prefilter.
    if (unicodeFallback) {
      for (let offset = 0; offset < text.length; offset += CHUNK) {
        if (/[^\x00-\x7f]/.test(text.slice(offset, offset + CHUNK))) return await unicodeFallback();
        await checkpoint(ctx, Math.ceil(CHUNK / 64));
      }
    }
    let selected = 0, lineNumber = 0, position = 0, lastYield = 0;
    while (position < text.length) {
      // Finding a newline and literals in bounded windows keeps timers responsive
      // even for minified single-line files. Do not split/retain the whole input.
      let lineEnd = position;
      while (lineEnd < text.length) {
        const end = Math.min(text.length, lineEnd + CHUNK);
        const newline = text.slice(lineEnd, end).indexOf("\n");
        if (newline >= 0) { lineEnd += newline; break; }
        lineEnd = end;
        await checkpoint(ctx, Math.ceil(CHUNK / 64));
      }
      const line = text.slice(position, lineEnd);
      lineNumber++;
      let first = await nextLiteral(line, template.literal, 0, ctx, line.length, !!unicodeFallback);
      const matches = first >= 0;
      if (matches !== parsed.invert) {
        selected++; any = true;
        if (parsed.quiet) return { stdout: "", stderr: "", exitCode: 0 };
        if (!parsed.count && !parsed.filesWith && !parsed.filesWithout) {
          const prefix = `${showName ? `${file === "-" ? "(standard input)" : file}:` : ""}${showNumber ? `${lineNumber}:` : ""}`;
          if (parsed.only && !parsed.invert) {
            let from = 0;
            while (first >= 0) {
              const start = backwards(line, first, template.before, from);
              const prefixEnd = forwards(line, start, template.before);
              let chosen = first;
              for (;;) {
                const next = await nextLiteral(line, template.literal, chosen + 1, ctx, prefixEnd, !!unicodeFallback);
                if (next < 0 || next > prefixEnd) break;
                chosen = next;
                if ((chosen - lastYield) >= CHUNK) { await checkpoint(ctx); lastYield = chosen; }
              }
              const end = forwards(line, chosen + template.literal.length, template.after);
              append(`${prefix}${line.slice(start, end)}\n`);
              from = end;
              first = await nextLiteral(line, template.literal, from, ctx, line.length, !!unicodeFallback);
            }
          } else if (!parsed.only) append(`${prefix}${line}\n`);
        }
        if (parsed.max > 0 && selected >= parsed.max) break;
      }
      position = lineEnd + 1;
      if (lineNumber % 128 === 0) await checkpoint(ctx);
    }
    if (parsed.filesWith || parsed.filesWithout) {
      if (parsed.filesWith && selected > 0 || parsed.filesWithout && selected === 0) append(`${file === "-" ? "(standard input)" : file}\n`);
    } else if (parsed.count) append(`${showName ? `${file === "-" ? "(standard input)" : file}:` : ""}${selected}\n`);
  }
  return { stdout: output.join(""), stderr: errors, exitCode: errors ? 2 : any ? 0 : 1 };
}

async function nextLiteral(line, literal, from, ctx, maxStart = line.length, ignoreAsciiCase = false) {
  const boundary = Math.min(line.length, maxStart + literal.length);
  for (let offset = from; offset <= maxStart; offset += CHUNK) {
    const end = Math.min(boundary, offset + CHUNK + literal.length - 1);
    const window = line.slice(offset, end);
    const found = (ignoreAsciiCase ? window.toLowerCase() : window).indexOf(literal);
    if (found >= 0) return offset + found;
    if (end === boundary) return -1;
    await checkpoint(ctx, Math.ceil(CHUNK / 64));
  }
  return -1;
}
function backwards(text, position, count, floor) {
  while (count-- > 0 && position > floor) {
    position--;
    const code = text.charCodeAt(position);
    if (code >= 0xDC00 && code <= 0xDFFF && position > floor && text.charCodeAt(position - 1) >= 0xD800 && text.charCodeAt(position - 1) <= 0xDBFF) position--;
  }
  return position;
}
function forwards(text, position, count) {
  while (count-- > 0 && position < text.length) position += text.codePointAt(position) > 0xFFFF ? 2 : 1;
  return position;
}
async function checkpoint(ctx, work = 1) {
  ctx.signal?.throwIfAborted();
  ctx.executionScope?.consumeWork(work, "search");
  ctx.executionScope?.throwIfAborted();
  await new Promise((resolve) => setTimeout(resolve, 0));
  ctx.signal?.throwIfAborted();
  ctx.executionScope?.throwIfAborted();
}
function decodeStdin(bytes) {
  const data = new Uint8Array(bytes.length);
  for (let i = 0; i < bytes.length; i++) data[i] = bytes.charCodeAt(i);
  return decoder.decode(data);
}
function fallbackCost(args) {
  const source = args.join(" ");
  if (source.length > 8192) return FALLBACK_WORK;
  let repeat = 1;
  for (const match of source.matchAll(/(?:\\)?\{(\d+)(?:,(\d*))?(?:\\)?\}/g)) {
    const count = Number(match[2] || match[1]);
    repeat *= Math.max(1, count);
    if (!Number.isSafeInteger(repeat) || repeat > 4096) return FALLBACK_WORK;
  }
  return Math.max(1, Math.min(FALLBACK_WORK, source.length * repeat));
}
function failure(name, reason) { return { stdout: "", stderr: `${name}: ${reason}\n`, exitCode: 126 }; }
// Resource admission is execution-fatal, not ordinary command status. A
// successful final pipeline command must not authorize following shell effects.
function refusal(ctx, name, reason) {
  chargeRefusal(ctx, name, reason);
  return failure(name, reason);
}
function chargeRefusal(ctx, name, reason) {
  try {
    ctx.executionScope?.consumeLimited("search_admission", 1, 0, `${name}: ${reason}`);
  } catch (error) {
    // Preserve upstream's fatal error identity while allowing local file-read
    // catches to distinguish admission from ordinary missing-file diagnostics.
    error.shellLimit = true;
    error.fatalSearchAdmission = true;
    throw error;
  }
}
function limit(ctx, name, reason) {
  chargeRefusal(ctx, name, reason);
  throw Object.assign(new Error(`${name}: ${reason}`), { shellLimit: true });
}

// Admission grammar for the installed upstream search parsers, not a second
// command parser. Unknown forms retain whole-argv costing and the old source
// file scan. In particular, rg does not recognize -- as an option terminator.
function searchArguments(name, args) {
  const legacy = () => ({ paths: patternFilePaths(args), costSources: args });
  const rg = name === "rg", paths = [], sources = [], operands = [];
  const flags = rg ? "isSFwxvUcloqnNHI0bLzau" : "invclLrRwxEPFohq";
  const longFlags = new Set((rg
    ? "ignore-case case-sensitive smart-case fixed-strings word-regexp line-regexp invert-match multiline multiline-dotall count count-matches files files-with-matches files-without-match stats only-matching quiet no-line-number line-number with-filename no-filename null byte-offset column no-column vimgrep json hidden no-ignore no-ignore-dot no-ignore-vcs follow search-zip text heading passthru include-zero glob-case-insensitive unrestricted"
    : "ignore-case line-number invert-match count files-with-matches files-without-match recursive word-regexp line-regexp extended-regexp perl-regexp fixed-strings only-matching no-filename quiet silent").split(" "));
  // Glob/type/preprocessor options can introduce other regex sources. Leave
  // those and unrecognized value options on the conservative legacy path.
  const rgValues = { regexp: "e", file: "f", "max-count": "m", replace: "r", "max-depth": "d", "max-filesize": "size", "context-separator": "separator", threads: "j" };
  const add = (flag, value) => {
    if (flag === "f") paths.push(value);
    else if (flag === "e") {
      if (!rg) sources.length = 0; // grep keeps only its last standalone -e.
      sources.push(value);
    }
  };
  let options = true, positional;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (!rg && options && arg === "--") { options = false; continue; }
    if (!options || !arg.startsWith("-") || arg === "-") {
      if (rg) {
        if (positional === undefined && sources.length === 0 && paths.length === 0) positional = arg;
      } else operands.push(arg);
      continue;
    }
    if (arg.startsWith("--")) {
      if (longFlags.has(arg.slice(2))) continue;
      const equal = arg.indexOf("="), key = arg.slice(2, equal < 0 ? undefined : equal);
      const flag = rg ? (Object.hasOwn(rgValues, key) ? rgValues[key] : undefined) : key === "file" ? "f" : key === "max-count" && equal >= 0 ? "m" : undefined;
      if (!flag) return legacy();
      const value = equal < 0 ? args[++i] : arg.slice(equal + 1);
      if (value === undefined) return legacy();
      add(flag, value); continue;
    }
    // Both parsers handle context counts before expanding short clusters.
    if (/^-[ABC]\d+$/.test(arg) || /^-m\d+$/.test(arg)) continue;
    if (["-A", "-B", "-C", "-m", "-e"].includes(arg)) {
      if (args[i + 1] === undefined) return legacy();
      add(arg[1], args[++i]); continue;
    }
    // rg first recognizes attached value options at the start of an argument.
    if (rg && "efmrdj".includes(arg[1]) && arg.length > 2) { add(arg[1], arg.slice(2)); continue; }
    for (let j = 1; j < arg.length; j++) {
      const flag = arg[j];
      if (flag === "f" && !rg) {
        const value = arg.slice(j + 1) || args[++i];
        if (value === undefined) return legacy();
        add(flag, value); break;
      }
      // Unlike grep's attached -f, each rg value flag within a cluster
      // consumes the next argv entry, then parsing resumes in that cluster.
      if (rg && "efmrdj".includes(flag)) {
        if (args[i + 1] === undefined) return legacy();
        add(flag, args[++i]); continue;
      }
      if (!flags.includes(flag)) return legacy();
    }
  }
  if (rg) { if (positional !== undefined) sources.push(positional); }
  else if (sources.length === 0 && paths.length === 0 && operands.length) sources.push(operands[0]);
  return { paths, sources };
}

function patternFilePaths(args) {
  const paths = [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--") break;
    let file;
    if (arg === "--file" || arg === "-f") file = args[++i];
    else if (arg.startsWith("--file=")) file = arg.slice(7);
    else {
      const short = arg.match(/^-[EFGPinvclLqshHorRwxa]*f(.*)$/);
      if (short) file = short[1] || args[++i];
    }
    if (typeof file !== "string") continue;
    paths.push(file);
  }
  return paths;
}

// Small source-option grammar. Unknown source-producing options route before
// parsing; the original interpreter retains ordinary execution semantics.
function programArguments(name, args) {
  const paths = [], sources = [];
  let options = true, haveProgram = false, separatorsSafe = true;
  const safeSeparator = (value) => typeof value === "string" && value.length <= 1 && !".*+?^${}()|[]\\".includes(value || "\0");
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (options && arg === "--") { options = false; continue; }
    if (options && arg.startsWith("-") && arg !== "-") {
      if (["--help", "--version", "-h", "-V"].includes(arg)) return args.length === 1 ? { paths, sources, help: true } : { uncertain: true };
      if (arg === "-f" || arg === "--file") { if (args[i + 1] === undefined) return { uncertain: true }; paths.push(args[++i]); haveProgram = true; continue; }
      if (arg.startsWith("--file=")) { paths.push(arg.slice(7)); haveProgram = true; continue; }
      if (name === "sed") {
        if (arg === "-e" || arg === "--expression") { if (args[i + 1] === undefined) return { uncertain: true }; sources.push(args[++i]); haveProgram = true; continue; }
        if (arg.startsWith("--expression=")) { sources.push(arg.slice(13)); haveProgram = true; continue; }
        const sourceOption = arg.match(/^-[nErzs]*([ef])(.*)$/);
        if (sourceOption) {
          const value = sourceOption[2] || args[++i];
          if (value === undefined) return { uncertain: true };
          if (sourceOption[1] === "f") paths.push(value); else sources.push(value);
          haveProgram = true; continue;
        }
        if (/^-[nErzsi]+$/.test(arg) || ["--quiet", "--silent", "--regexp-extended", "--posix", "--in-place"].includes(arg) || arg.startsWith("--in-place=")) continue;
      } else {
        if (arg.startsWith("-f") && arg.length > 2) { paths.push(arg.slice(2)); haveProgram = true; continue; }
        if (arg === "-F" || arg === "--field-separator") { separatorsSafe &&= safeSeparator(args[++i]); continue; }
        if (arg.startsWith("-F") && arg.length > 2) { separatorsSafe &&= safeSeparator(arg.slice(2)); continue; }
        if (arg.startsWith("--field-separator=")) { separatorsSafe &&= safeSeparator(arg.slice(18)); continue; }
        if (arg === "-v") {
          const value = args[++i]; if (typeof value !== "string") return { uncertain: true };
          if (/^(FS|RS|FPAT|FIELDWIDTHS)=/.test(value)) separatorsSafe &&= !/^(FPAT|FIELDWIDTHS)=/.test(value) && safeSeparator(value.slice(value.indexOf("=") + 1));
          continue;
        }
      }
      return { uncertain: true };
    }
    if (!haveProgram) { sources.push(arg); haveProgram = true; }
    else if (name === "awk" && /^(FS|RS|FPAT|FIELDWIDTHS)=/.test(arg)) separatorsSafe &&= !/^(FPAT|FIELDWIDTHS)=/.test(arg) && safeSeparator(arg.slice(arg.indexOf("=") + 1));
  }
  return { paths, sources, separatorsSafe };
}
function programPolicy(name, programs, plan) {
  if (plan.help) return { safe: true };
  if (name === "sed") {
    // Numeric addressing and these no-argument operations cannot compile a
    // user regex. Other sed programs use conservative regex work admission.
    const safe = programs.every((source) => source.split(/[;\n]/).every((part) => !part.trim()
      || /^\s*(?:(?:\d+(?:\s*~\s*\d+)?|\$)(?:\s*,\s*(?:\+?\d+|\$))?\s*)?!?\s*[pdnNhHgGx=DPlq]\s*\d*\s*$/.test(part)));
    // Recognize only one complete slash-delimited substitution with a nonempty
    // pattern and ordinary flags. Replacement braces are literal output, not
    // repetition counts. Empty-pattern reuse, addresses, extra commands and
    // other delimiters retain the conservative whole-program cost. Bracket
    // expressions also stay conservative: upstream ignores slash delimiters
    // inside them, so a delimiter-only scan cannot identify their replacement.
    // Keep the full source length as a work floor at the call site.
    const costSources = programs.map((source) => {
      const substitution = source.match(/^\s*s\/((?:\\[^\r\n]|[^/\\\r\n])+)\/((?:\\[^\r\n]|[^/\\\r\n])*)\/[gp]*\s*$/);
      return substitution && !substitution[1].includes("[") ? substitution[1] : source;
    });
    return { safe, costSources };
  }
  if (!plan.separatorsSafe) return { uncertain: true };
  const noRegex = (source) => !/[\/~@|]/.test(source)
    && !/\b(?:sub|gsub|match|split|gensub|patsplit|FS|RS|FPAT|FIELDWIDTHS|ARGV|ARGC|ENVIRON|getline|system|eval)\b/.test(source);
  if (programs.every(noRegex)) return { safe: true };
  // Only a static record regex plus a non-regex action is admitted. Dynamic
  // operands, regex builtins and programmable separators route before compile.
  const staticPatterns = programs.every((source) => {
    const pattern = source.match(/^\s*(?:\$\d+\s*~\s*)?\/((?:\\.|[^/])*)\/\s*(\{[\s\S]*\})?\s*$/);
    return pattern && noRegex(pattern[2] ?? "");
  });
  return staticPatterns ? {} : { uncertain: true };
}
