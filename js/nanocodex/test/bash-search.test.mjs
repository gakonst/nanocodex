import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { justBash } from "../tools/bash.mjs";
import { Bash } from "nanocodex-tools/just-bash/browser";

// An OS child is essential: node:test timeouts cannot interrupt a synchronous
// RegExp that blocks the isolate. Every journey has a parent-owned SIGKILL fence.
const childCase = process.env.NANOCODEX_SEARCH_CHILD;
if (childCase) {
  try {
    await getJourneys()[childCase]();
    trace({ journey: childCase, status: "passed" });
  } catch (error) {
    console.error(error.stack);
    process.exitCode = 1;
  }
} else {
  for (const [name, description] of [
    ["semantics", "literal and bounded-dot searches preserve actual Just Bash semantics"],
    ["fallback", "unsupported flags and regexes preserve upstream errors and output"],
    ["searchOperands", "fallback admission distinguishes search sources from data operands"],
    ["smartCase", "lowercase rg literals scan cooperatively and preserve Unicode smart-case"],
    ["oneMiB", "1 MiB bounded searches yield to timers and preserve results"],
    ["thirteenMiB", "13 MiB bounded searches yield to timers and recover with echo"],
    ["cancellation", "large search cancellation and deadlines leave the shell usable"],
    ["admission", "expensive fallback regex is rejected before blocking the isolate"],
    ["hostBoundary", "lazy loading and remote refresh cancel and recover without stale state"],
    ["sourceBudget", "pattern file effective cost includes stdin and aggregate data budget"],
    ["fatalAdmission", "resource refusal survives pipelines and prevents following effects"],
    ["streamRegex", "sed and awk regex/script admission precedes compile and recovers"],
    ["streamSemantics", "sed and awk ordinary text operations retain upstream results"],
    ["sedAdmission", "numeric sed addresses and literal replacement text retain upstream results"],
    ...(process.env.NANOCODEX_SEARCH_PUBLIC_FIXTURE ? [["publicFixture", "exact public CLI fixture agrees with native rg under independent deadline"]] : []),
  ]) {
    test(description, { timeout: 35_000 }, async (t) => {
      const result = await runChild(name, 30_000);
      const evidence = { journey: name, ...result };
      if (process.env.NANOCODEX_SEARCH_EVIDENCE) {
        await mkdir(process.env.NANOCODEX_SEARCH_EVIDENCE, { recursive: true });
        await writeFile(join(process.env.NANOCODEX_SEARCH_EVIDENCE, `${name}.json`),
          JSON.stringify(evidence, null, 2) + "\n");
      }
      t.diagnostic(result.stdout.trim());
      assert.equal(result.timedOut, false,
        `${name}: OS child exceeded 30 seconds; search may block the event loop\n${result.stderr}`);
      assert.equal(result.code, 0, `${name}: ${result.stderr}\n${result.stdout}`);
    });
  }
}

// Function declarations avoid an initialization race in the child branch above.
function trace(value) { console.log(JSON.stringify(value)); }
function context(signal = new AbortController().signal) {
  return { sessionId: "synthetic-search-regression", signal };
}
function quote(value) { return `'${value.replaceAll("'", "'\\''")}'`; }
function publicResult(value) { return { output: value.output, exit_code: value.exit_code }; }

function runChild(name, timeoutMs) {
  return new Promise((resolve, reject) => {
    const startedAt = performance.now();
    const child = spawn(process.execPath, [fileURLToPath(import.meta.url)], {
      env: { ...process.env, NANOCODEX_SEARCH_CHILD: name },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "", stderr = "", timedOut = false;
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    const fence = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, timeoutMs);
    child.on("error", (error) => { clearTimeout(fence); reject(error); });
    child.on("close", (code, signal) => {
      clearTimeout(fence);
      resolve({ code, signal, timedOut, elapsedMs: Math.round(performance.now() - startedAt), stdout, stderr });
    });
  });
}

async function pair(files) {
  const runtime = await justBash({ filesystem: memoryWorkspace(), maxOutputTokens: 100_000 });
  const baseline = new Bash({ cwd: "/workspace" });
  await baseline.fs.mkdir("/workspace", { recursive: true });
  for (const [path, contents] of Object.entries(files)) {
    await runtime.filesystem.writeFile(path, contents);
    await baseline.fs.mkdir(`/workspace/${path.split("/").slice(0, -1).join("/")}`, { recursive: true });
    await baseline.fs.writeFile(`/workspace/${path}`, contents);
  }
  trace({ fixtures: Object.fromEntries(Object.entries(files).map(([path, value]) =>
    [path, typeof value === "string" ? value : Array.from(value)])) });
  return { runtime, baseline, mismatches: [] };
}

async function compare(pair, cmd) {
  const raw = await pair.baseline.exec(cmd);
  const expected = { output: raw.stdout + raw.stderr, exit_code: raw.exitCode };
  const observed = publicResult(await pair.runtime.tool.handler({ cmd }, context()));
  trace({ cmd, expected, observed });
  try { assert.deepEqual(observed, expected, cmd); }
  catch (error) { pair.mismatches.push(error.message); }
}

async function semantics() {
  const shells = await pair({
    "hits.txt": "\nxxMARKmidMARKyy\nMARK MARKMARK end\nnone\nMARK\n\n",
    "other.txt": "missing\nMARKagain\n",
    "dir/nested.txt": "MARKinside\nnone\n",
    "miss.txt": "no marker\n",
    "unterminated.txt": "MARK\nlastMARK",
    "empty.txt": "",
    "unicode.txt": "é💡MARK💡尾\n𐐀MARK𐐀\n💡💡MARKMARK💡\n",
    "separators.txt": "x\u2028MARK\u2029y\nx\tMARK\ty\n",
    "surrogates.txt": "\ud800MARK\udc00\n💡MARK💡\n",
    "punctuation.txt": "file.name +-[x]\nfileXname\n",
    "binary.dat": new Uint8Array([77, 65, 82, 75, 0, 255, 13, 10, 77, 65, 82, 75]),
    "line-boundaries.txt": "before\nMARK\nafter\r\nMARK\r\n\n",
    "greedy.txt": "abMARKMARKcd\naaaaaMARKxMARKbb\nMARKMARKMARK\n",
    "overlap.txt": "aaaaaa\n💡💡💡💡💡\n",
    "smart-case.txt": "FOO\nfoo\nfOo\nFoo\n",
  });
  for (const command of ["grep", "fgrep", "egrep", "rg"]) {
    for (const flags of ["", "-o", "-n", "-on", "-c", "-m 1", "-m 2 -o", "-m 0", "-l", "-L", "-q", "-h", "-H", "-v", "-vo", "-nc", "-ql", "-cl", "-lL", "-m1 -on", "--only-matching", "--line-number", "--count", "--files-with-matches", "--files-without-match", "--quiet", "--no-filename", "--with-filename"]) {
      await compare(shells, `${command} ${flags} MARK hits.txt other.txt miss.txt`);
    }
    for (const file of ["hits.txt", "miss.txt", "unterminated.txt", "empty.txt", "binary.dat", "unicode.txt", "surrogates.txt"]) {
      await compare(shells, `${command} -on MARK ${file}`);
    }
    await compare(shells, `printf 'MARK\\nno\\nMARK' | ${command} -on MARK`);
    await compare(shells, `${command} -- MARK hits.txt`);
    await compare(shells, `${command} -o MARK hits.txt absent.txt`);
    await compare(shells, `${command} -o MARK dir`);
    await compare(shells, `${command} -o MARK dir hits.txt`);
  }
  // Bounded contexts must implement leftmost greedy competition, not select
  // the first marker and simply take a fixed slice around it.
  for (const [command, pattern] of [
    ["grep", ".\\{0,4\\}MARK.\\{0,3\\}"],
    ["grep -E", ".{0,4}MARK.{0,3}"],
    ["egrep", ".{0,4}MARK.{0,3}"],
    ["rg", ".{0,4}MARK.{0,3}"],
  ]) {
    for (const flags of ["-o", "-on", "-c", "-m 1 -o", "-q", ""]) {
      await compare(shells, `${command} ${flags} ${quote(pattern)} greedy.txt hits.txt`);
    }
  }
  for (const command of ["fgrep", "grep -F", "rg -F"]) {
    await compare(shells, `${command} -o 'file.name' punctuation.txt`);
    await compare(shells, `${command} -o '+-[x]' punctuation.txt`);
  }
  for (const bound of [0, 1, 2, 5]) {
    await compare(shells, `grep -o ${quote(`.\\{0,${bound}\\}MARK.\\{0,${bound}\\}`)} unicode.txt`);
    await compare(shells, `rg -o ${quote(`.{0,${bound}}MARK.{0,${bound}}`)} unicode.txt`);
  }
  await compare(shells, "rg -o '.{0,1}💡.{0,1}' unicode.txt");
  await compare(shells, "grep -E -o '.{0,1}MARK.{0,1}' surrogates.txt");
  await compare(shells, "grep -E -o '.{0,1}MARK.{0,1}' separators.txt");
  await compare(shells, "rg -o '.{0,1}MARK.{0,1}' separators.txt");
  for (const command of ["grep -E", "egrep", "rg"]) {
    for (const pattern of [".{0,50}MARK.{0,50}", ".{0,50}MISSING.{0,50}", ".{0,2}MARK", "MARK.{0,2}"]) {
      await compare(shells, `${command} -on ${quote(pattern)} line-boundaries.txt`);
    }
  }
  await compare(shells, "grep -E -o '.{0,2}aa.{0,1}' overlap.txt");
  await compare(shells, "rg -o '.{0,2}💡.{0,1}' overlap.txt");
  await compare(shells, "fgrep -o '.{0,4}MARK.{0,3}' hits.txt");
  for (const cmd of ["rg -o foo smart-case.txt", "rg -F -o foo smart-case.txt", "rg -o Foo smart-case.txt", "grep -o foo smart-case.txt", "grep -F -o foo smart-case.txt"]) await compare(shells, cmd);
  assert.deepEqual(shells.mismatches, [], "upstream semantic mismatches");
}

async function smartCase() {
  const shells = await pair({
    "case.txt": "foo FOO fOo Foo\nnone\n",
    "unicode-case.txt": "foo FOO\nİi ıI kKK sSſ σΣς éÉ ßẞ 𐐀𐐨\nK\nſ\nς\nİ\n",
    "report{5000}.txt": "alpha\n",
  });
  for (const pattern of ["foo", "Foo", "i", "k", "s", "σ", "é", "É", "ß", "𐐨"]) {
    for (const flags of ["-on", "-Fon", "-n", "-vn", "-l", "-q", "-ion", "-iFon", "-ivn", "-il", "--ignore-case -q"]) {
      await compare(shells, `rg ${flags} ${quote(pattern)} case.txt unicode-case.txt`);
    }
  }
  await compare(shells, "printf 'FOO foo\\n' | rg -on foo");
  await compare(shells, "sed 's/alpha/delta/' 'report{5000}.txt'");
  await compare(shells, "awk '/alpha/ {print}' 'report{5000}.txt'");
  // One long line crosses scanning windows; preserve original matched casing.
  const text = "x".repeat(65534) + "fOo" + "x".repeat(1024 * 1024) + "FOO\n";
  await shells.runtime.filesystem.writeFile("large-case.txt", text);
  await shells.baseline.fs.writeFile("/workspace/large-case.txt", text);
  trace({ fixture: "large-case.txt", bytes: text.length, generation: "x^65534 + fOo + x^1048576 + FOO + newline" });
  for (const cmd of ["rg -on foo large-case.txt", "rg -Fon foo large-case.txt", "rg -on '.{0,2}foo.{0,2}' large-case.txt", "rg -on Foo large-case.txt", "rg -o missing large-case.txt", "rg -ion FOO large-case.txt", "rg --ignore-case -oF Foo large-case.txt", "rg -ion '.{0,2}FOO.{0,2}' large-case.txt"]) {
    let ticks = 0;
    const timer = setInterval(() => ticks++, 0);
    try { await compare(shells, cmd); } finally { clearInterval(timer); }
    trace({ cmd, timerTicks: ticks });
    assert.ok(ticks > 1, "large literal matching must yield repeatedly");
  }
  assert.deepEqual(shells.mismatches, [], "smart-case and stream filename mismatches");
  // Unicode still uses the upstream matcher and its conservative admission.
  // Do not silently relax that budget merely to accelerate ASCII literals.
  await shells.runtime.filesystem.writeFile("large-unicode.txt", text + "İ\n");
  const refused = publicResult(await shells.runtime.tool.handler({ cmd: "rg -io FOO large-unicode.txt" }, context()));
  trace({ cmd: "rg -io FOO large-unicode.txt", observed: refused, expected: "Unicode fallback retains admission" });
  assert.equal(refused.exit_code, 126);
  assert.match(refused.output, /admission/);
  const cancellation = new AbortController();
  const running = shells.runtime.tool.handler({ cmd: "rg -io FOO large-case.txt" }, context(cancellation.signal));
  const timer = setTimeout(() => cancellation.abort(new Error("cancel ASCII scan")), 1);
  const cancelled = await running;
  clearTimeout(timer);
  trace({ cmd: "rg -io FOO large-case.txt", cancelled });
  assert.equal(cancelled.exit_code, 124);
  await recovery(shells.runtime);
}

async function fallback() {
  const shells = await pair({ "patterns.txt": "MARK\nmark\n", "input.txt": "mark\nMARK12\nMARK\npreMARKpost\n\n", "-dash.txt": "MARK\n" });
  for (const command of ["grep", "fgrep", "egrep", "rg"]) {
    for (const flags of ["--not-a-search-option", "-Z", "-i", "-v", "-w", "-x", "-A 1", "-B 1", "-C 1", "-e MARK -e mark", "-m", "-m nope"]) {
      await compare(shells, `${command} ${flags} MARK input.txt`);
    }
    for (const pattern of ["", "MARK\npreMARK", "^MARK$", "MARK|mark", "[Mm]ARK", "MARK[0-9]+", "MARK.*", ".{1,3}MARK", ".{0,2}MARK?", "["]) {
      await compare(shells, `${command} -o ${quote(pattern)} input.txt`);
    }
    await compare(shells, `${command} -o -e MARK -- -dash.txt`);
    await compare(shells, `${command} -f absent-patterns.txt input.txt`);
    if (command !== "rg") {
      await compare(shells, `${command} -f patterns.txt input.txt`);
      await compare(shells, `${command} -vfpatterns.txt input.txt`);
      await compare(shells, `${command} --file=patterns.txt input.txt`);
    }
  }
  await compare(shells, "grep -E -o '(MARK|mark)' input.txt");
  await compare(shells, "grep -o 'MARK\\{1,2\\}' input.txt");
  await compare(shells, "rg -o '(?<=pre)MARK' input.txt");
  for (const command of ["grep", "fgrep", "egrep", "rg"]) {
    await compare(shells, `${command} -o MARK .`);
  }
  await compare(shells, "echo fallback-recovered");
  assert.deepEqual(shells.mismatches, [], "upstream fallback mismatches");
}

async function searchOperands() {
  const shells = await pair({
    "report{5000}.txt": "foo\nFOO\nbar\n",
    "plain.txt": "foo\nFOO\nbar\n",
    "patterns{5000}.txt": "foo\n",
    "other-patterns.txt": "bar\n",
    "-report{5000}.txt": "foo\n",
  });
  for (const command of ["grep", "fgrep", "egrep", "rg"]) {
    for (const args of [
      "-i foo", "-in foo", "--ignore-case foo", "-A 1 foo", "-B1 foo", "-C 1 -e foo",
      "-m1 -i foo", "--max-count=1 -i foo", "-e foo -i", "-e bar -e foo -i",
      "-i -f 'patterns{5000}.txt'", "-if 'patterns{5000}.txt'",
      "--file=patterns{5000}.txt -i", "-if 'patterns{5000}.txt' -f other-patterns.txt",
    ]) {
      await compare(shells, command + " " + args + " 'report{5000}.txt'");
    }
    await compare(shells, command + " -i foo plain.txt");
    if (command !== "rg") await compare(shells, command + " 'report{5000}.txt' -i -e foo");
    await compare(shells, command + " --not-a-search-option foo plain.txt");
    if (command !== "rg") {
      await compare(shells, command + " -ifpatterns{5000}.txt 'report{5000}.txt'");
      await compare(shells, command + " -i -- foo '-report{5000}.txt'");
      await compare(shells, command + " -i -e foo -- '-report{5000}.txt'");
    }
  }
  for (const args of [
    "-ie foo", "-efoo -i", "--regexp=foo -i", "--regexp foo -i",
    "-ife 'patterns{5000}.txt' bar", "-ifn 'patterns{5000}.txt'",
    "-ffoo", // Attached -f means a missing pattern file named foo.
    "-i --replace='{5000}' foo", "-ir '{5000}' foo",
    "-i --context-separator='{5000}' -A 1 foo",
  ]) await compare(shells, "rg " + args + " 'report{5000}.txt'");
  await compare(shells, "printf 'foo\\n' | rg -if - 'report{5000}.txt'");
  assert.deepEqual(shells.mismatches, [], "search source extraction must preserve upstream results");

  const { runtime, reads } = await tracedShell({ executionTimeoutMs: 100 });
  const hostile = "(a{65535}){65535}";
  await runtime.filesystem.writeFile("hostile", hostile);
  await runtime.filesystem.writeFile("safe", "foo");
  await runtime.filesystem.writeFile("input.txt", "foo\n");
  await runtime.filesystem.writeFile("costly", "a{1000}Z");
  await runtime.filesystem.writeFile("budget.txt", "a".repeat(400));
  const commands = [
    "rg -i " + quote(hostile) + " input.txt",
    "rg -e" + quote(hostile) + " input.txt",
    "rg --regexp=" + quote(hostile) + " input.txt",
    "rg -ie " + quote(hostile) + " input.txt",
    "rg -ife safe " + quote(hostile) + " input.txt",
    "rg foo -ie " + quote(hostile) + " input.txt",
    "rg -ifn hostile input.txt", "rg -fhostile input.txt",
    "rg -ief foo hostile input.txt", "rg --file=hostile input.txt",
    "printf '%s' " + quote(hostile) + " | rg -if - input.txt",
    "grep -E -ifhostile input.txt", "egrep -if hostile input.txt",
    "grep -E -e " + quote(hostile) + " input.txt",
    "grep -E -- " + quote(hostile) + " input.txt",
    // Unknown forms retain the previous whole-argv conservative refusal.
    "rg --unknown foo 'report{5000}.txt'",
    "rg --constructor foo 'report{5000}.txt'",
    "rg 'report{5000}.txt' -i -e foo",
    "rg --glob '*.txt' " + quote(hostile) + " input.txt",
    // Known -f clusters and inline sources still charge aggregate data bytes.
    "rg -ifn costly budget.txt", "rg -ie 'a{1000}Z' budget.txt",
    "grep -E -ifcostly budget.txt",
  ];
  for (const command of commands) {
    reads.length = 0;
    const cmd = command + " | head -c 1; echo forbidden > forbidden.txt";
    const started = performance.now();
    const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
    const elapsedMs = Math.round(performance.now() - started);
    trace({ cmd, expected: "fatal admission before data read; no subsequent write", observed, reads: [...reads], elapsedMs });
    assert.equal(observed.exit_code, 126, cmd);
    assert.match(observed.output, /admission.*native Hand/);
    assert.ok(!reads.includes("/workspace/input.txt") && !reads.includes("/workspace/budget.txt"), "admission precedes data read");
    await assert.rejects(runtime.filesystem.readFile("forbidden.txt"));
    assert.ok(elapsedMs < 1000, cmd);
    await recovery(runtime);
  }
}

async function largeScan(size) {
  const runtime = await justBash({ filesystem: memoryWorkspace() });
  const input = "a".repeat(size - 108) + "MARKER" + "b".repeat(102);
  await runtime.filesystem.writeFile("large.txt", input);
  trace({ fixture: "large.txt", bytes: size, generation: "a^(size-108) + MARKER + b^102" });
  for (const cmd of [
    "grep -o '.\\{0,50\\}MARKER.\\{0,50\\}' large.txt",
    "rg -o '.{0,50}MARKER.{0,50}' large.txt",
    "egrep -o '.{0,50}MISSING.{0,50}' large.txt",
    "fgrep -o MARKER large.txt",
  ]) {
    let timerRanDuringExecution = false, pending = true;
    const startedAt = performance.now();
    const running = runtime.tool.handler({ cmd }, context());
    const timer = setTimeout(() => { timerRanDuringExecution = pending; }, 0);
    const observed = publicResult(await running);
    pending = false;
    clearTimeout(timer);
    const expected = cmd.includes("MISSING")
      ? { output: "", exit_code: 1 }
      : { output: cmd.startsWith("fgrep") ? "MARKER\n"
        : "a".repeat(50) + "MARKER" + "b".repeat(50) + "\n", exit_code: 0 };
    const elapsedMs = Math.round(performance.now() - startedAt);
    trace({ cmd, bytes: size, expected, observed, timerRanDuringExecution, elapsedMs });
    assert.deepEqual(observed, expected, cmd);
    assert.equal(timerRanDuringExecution, true, `${cmd}: timer starved during ${size}-byte scan`);
    assert.ok(elapsedMs < 10_000, `${cmd}: took ${elapsedMs}ms`);
  }
  await recovery(runtime);
}

async function tracedShell(options = {}) {
  const source = memoryWorkspace(), reads = [];
  const original = source.readFile.bind(source);
  source.readFile = async (path) => { reads.push(path); return original(path); };
  const runtime = await justBash({ filesystem: source, ...options });
  return { runtime, reads };
}
async function sourceBudget() {
  const { runtime, reads } = await tracedShell({ executionTimeoutMs: 100 });
  await runtime.filesystem.writeFile("p", "a{4000}Z");
  await runtime.filesystem.writeFile("stdin.txt", "a".repeat(100000));
  await runtime.filesystem.writeFile("p-small", "a{1000}Z");
  // Repeated cached data reads must exceed the actual source-work budget,
  // without relying on filenames inflating regex cost.
  await runtime.filesystem.writeFile("repeat-p", "a{100}Z");
  await runtime.filesystem.writeFile("tiny-stdin.txt", "a".repeat(100));
  await runtime.filesystem.writeFile("tiny-file.txt", "a".repeat(150));
  for (const cmd of ["cat stdin.txt | grep -E -f p", "cat tiny-stdin.txt | grep -E -f p-small tiny-file.txt -", "grep -E -f repeat-p " + Array(800).fill("repeat-p").join(" ")]) {
    reads.length = 0;
    const started = performance.now();
    const observed = await runtime.tool.handler({ cmd }, context());
    trace({ cmd, observed, reads: [...reads], elapsedMs: performance.now() - started, expected: "final effective pattern cost includes all stdin/file bytes" });
    assert.notEqual(observed.exit_code, 0);
    assert.match(observed.output, /admission.*native Hand/);
    assert.ok(performance.now() - started < 1000);
    assert.ok(!reads.includes("/workspace/tiny-file.txt"), "aggregate data admission precedes data read");
    await recovery(runtime);
  }
}
async function fatalAdmission() {
  const { runtime, reads } = await tracedShell({ executionTimeoutMs: 100 });
  await runtime.filesystem.writeFile("p", "a{4000}Z");
  await runtime.filesystem.writeFile("large.txt", "a".repeat(1024 * 1024));
  await runtime.filesystem.writeFile("script.awk", "/(a{65535}){65535}/ {print}");
  await runtime.filesystem.writeFile("long.sed", "p".repeat(8193));
  const rejected = [
    "grep -E -f p <<'EOF'\n" + "a".repeat(100000) + "\nEOF\n",
    "grep -E '(a{65535}){65535}' large.txt",
    "sed -n '/.\\{0,250\\}x-anthropic-billing-header.\\{0,500\\}/p' large.txt",
    "awk '$0 ~ /.{0,250}x-anthropic-billing-header.{0,500}/ {print}' large.txt",
    "awk -f script.awk large.txt",
    "sed -f long.sed large.txt",
    "awk '$0 ~ $1 {print}' large.txt",
    "awk -F '.{0,4000}' '{print}' large.txt",
  ];
  const masked = [];
  for (let index = 0; index < rejected.length; index++) {
    const source = rejected[index];
    const marker = `refusal-marker-${index}`;
    const cmd = source.includes("<<")
      ? source.replace("<<'EOF'", `<<'EOF' | head -c 1 && printf unauthorized-follow > ${marker}`)
      : source + ` | head -c 1 && printf unauthorized-follow > ${marker}`;
    reads.length = 0;
    const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
    const exists = (await runtime.filesystem.list()).some(entry => entry.path === `/workspace/${marker}`);
    trace({ fixture: "a^100000 stdin or a^1048576 file", source: source.slice(0, 180), suffix: "| head -c 1 && printf unauthorized-follow > marker", observed, markerExists: exists, reads: [...reads] });
    if (observed.exit_code === 0 || exists) masked.push({ index, observed, markerExists: exists });
    assert.match(observed.output, /admission.*native Hand/);
    assert.ok(!reads.includes("/workspace/large.txt"), "refusal precedes data read");
    await recovery(runtime);
  }
  for (const separator of [";", "||"]) {
    const marker = separator === ";" ? "unconditional-marker" : "fallback-marker";
    const cmd = `awk '$0 ~ $1 {print}' large.txt | head -c 1 ${separator} printf unauthorized-follow > ${marker}`;
    const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
    const exists = (await runtime.filesystem.list()).some(entry => entry.path === `/workspace/${marker}`);
    trace({ cmd, observed, markerExists: exists });
    assert.equal(observed.exit_code, 126);
    assert.equal(exists, false, "resource refusal must stop the whole shell execution, not only &&");
    await recovery(runtime);
  }
  for (const mode of ["input", "output"]) {
    const source = memoryWorkspace();
    await source.writeFile("/workspace/big.txt", "MARK".repeat(256));
    const limited = await justBash({ filesystem: source, executionLimits: mode === "input" ? { maxInputBytes: 512 } : { maxOutputSize: 512 } });
    const cmd = "grep MARK big.txt | head -c 1 && printf unauthorized-follow > optimized-marker";
    const observed = publicResult(await limited.tool.handler({ cmd }, context()));
    const exists = (await limited.filesystem.list()).some(entry => entry.path === "/workspace/optimized-marker");
    trace({ mode, fixtureBytes: 1024, cmd, observed, markerExists: exists });
    assert.notEqual(observed.exit_code, 0);
    assert.equal(exists, false);
    assert.match(observed.output, /limit exceeded/);
    await recovery(limited);
  }
  assert.deepEqual(masked, [], "resource refusal must escape pipelines and block following effects");
  // Ordinary command status is still Bash status: upstream missing-file errors
  // may be masked by a successful final pipeline command without pipefail.
  for (const cmd of ["grep -E -f absent-patterns large.txt | head -c 1 && printf ordinary > ordinary-marker", "grep missing large.txt | head -c 1 && printf ordinary > ordinary-marker"]) {
    const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
    trace({ cmd, observed, markerExists: (await runtime.filesystem.list()).some(entry => entry.path === "/workspace/ordinary-marker") });
    assert.equal(observed.exit_code, 0);
    assert.equal((await runtime.filesystem.list()).some(entry => entry.path === "/workspace/ordinary-marker"), true);
    await recovery(runtime);
  }
}
async function streamRegex() {
  const { runtime, reads } = await tracedShell({ executionTimeoutMs: 100 });
  const marker = "x-anthropic-billing-header";
  const input = "a".repeat(1024 * 1024 - 31) + marker + "bbbbbb";
  await runtime.filesystem.writeFile("large.txt", input);
  const sed = "/.\\{0,250\\}x-anthropic-billing-header.\\{0,500\\}/p";
  const awk = "$0 ~ /.{0,250}x-anthropic-billing-header.{0,500}/ { print substr($0, 1, 3500) }";
  await runtime.filesystem.writeFile("script.sed", sed);
  await runtime.filesystem.writeFile("script.awk", awk);
  await runtime.filesystem.writeFile("huge.sed", "s/a{65535}/x/g");
  await runtime.filesystem.writeFile("huge.awk", "/(a{65535}){65535}/ {print}");
  await runtime.filesystem.writeFile("dynamic.awk", "$0 ~ $1 {print}");
  await runtime.filesystem.writeFile("long.sed", "p".repeat(8193));
  await runtime.filesystem.writeFile("long.awk", " ".repeat(8193));
  const commands = [
    "sed -n " + quote(sed) + " large.txt",
    "set -o pipefail; sed -n " + quote(sed) + " large.txt | head -c 3500",
    "awk " + quote(awk) + " large.txt",
    "sed -nf script.sed large.txt", "sed -nfscript.sed large.txt", "awk -f script.awk large.txt",
    "cat script.sed | sed -n -f - large.txt", "cat script.awk | awk -f - large.txt",
    "sed -E -f huge.sed large.txt", "awk -f huge.awk large.txt", "awk -f dynamic.awk large.txt",
    "awk '$0 ~ $1 {print}' large.txt", "awk -F '.{0,4000}' '{print}' large.txt",
    "sed -f long.sed large.txt", "awk -f long.awk large.txt", "cat long.awk | awk -f - large.txt",
  ];
  for (const cmd of commands) {
    reads.length = 0;
    const started = performance.now();
    const observed = await runtime.tool.handler({ cmd }, context());
    trace({ cmd, fixtureBytes: input.length, configuredDeadlineMs: 100, observed, reads: [...reads], elapsedMs: performance.now() - started });
    assert.notEqual(observed.exit_code, 0, cmd);
    assert.match(observed.output, /admission.*native Hand/);
    assert.ok(!reads.includes("/workspace/large.txt"), "guard must precede regex data read");
    assert.ok(performance.now() - started < 1000);
    await recovery(runtime);
  }
  // Real deadlines are observable at the host-yield boundary; they are not a
  // substitute for the preceding precompile admission.
  const aborted = new AbortController(); aborted.abort(new Error("fixture cancellation"));
  assert.equal((await runtime.tool.handler({ cmd: "awk '{print}' large.txt" }, context(aborted.signal))).exit_code, 124);
  await recovery(runtime);
  // The 100 ms deadline above bounds the refusals; streaming 1 MiB through
  // sed/awk is ordinary work, so it runs under a realistic deadline (a loaded
  // CI runner exceeded 100 ms here: run 38023489366).
  const { runtime: ordinary } = await tracedShell({ executionTimeoutMs: 10_000 });
  await ordinary.filesystem.writeFile("large.txt", input);
  const safeSed = await ordinary.tool.handler({ cmd: "sed -n '1p' large.txt | head -c 5" }, context());
  assert.equal(safeSed.output, "aaaaa"); assert.equal(safeSed.exit_code, 0);
  const safeAwk = await ordinary.tool.handler({ cmd: "awk '{print length($0)}' large.txt" }, context());
  assert.equal(safeAwk.output, input.length + "\n"); assert.equal(safeAwk.exit_code, 0);
  trace({ safeSed, safeAwk, expected: "nonregex text operations remain available on 1MiB input" });
}
async function streamSemantics() {
  const shells = await pair({ "input.txt": "alpha 1\nbeta 2\nalpha 3\n", "script.sed": "1,2p", "script.awk": "{ sum += $2 } END {print sum}" });
  for (const cmd of ["sed -n '1,2p' input.txt", "sed '2d' input.txt", "sed '1q' input.txt", "sed 's/alpha/ALPHA/g' input.txt", "sed -n '/alpha/p' input.txt", "sed -n -f script.sed input.txt", "awk '{print $1}' input.txt", "awk '{sum += $2} END {print sum}' input.txt", "awk -F ' ' '{print $2}' input.txt", "awk '/alpha/ {print $2}' input.txt", "awk -f script.awk input.txt", "printf 'alpha 1\\nbeta 2\\n' | awk '{print $2}'"]) await compare(shells, cmd);
  assert.deepEqual(shells.mismatches, [], "sed/awk upstream semantic mismatches");
}

async function sedAdmission() {
  const shells = await pair({
    "input.txt": "alpha 1\nbeta 2\nalpha 3\n",
    "replace.sed": "s/alpha/(a{65535}){65535}/g\n",
    "bracket.txt": "/aaaa\n",
  });
  // A large data file makes accidental regex admission of numeric addresses
  // visible. Compare the public handler against the real upstream interpreter.
  const input = "alpha " + "a".repeat(600_000) + "\nbeta\ngamma\n";
  await shells.runtime.filesystem.writeFile("large.txt", input);
  await shells.baseline.fs.writeFile("/workspace/large.txt", input);
  trace({ fixture: "large.txt", bytes: input.length, generation: "alpha + a^600000 + newline beta newline gamma newline" });
  for (const cmd of [
    "sed -n '1,+1p' large.txt | head -c 40",
    "sed -n '1~2p' large.txt | head -c 40",
    "sed 's/alpha/(a{65535}){65535}/g' input.txt",
    "sed -f replace.sed input.txt",
    "sed -E 's/[/](a{2}){2}/g' bracket.txt",
    "sed 's/alpha/literal\\/{5000};still replacement/g' input.txt",
    "sed -e 's/alpha/{5000}/' -e 's/beta/{6000}/' input.txt",
    "sed -n '1,+1!p' input.txt",
  ]) await compare(shells, cmd);
  assert.deepEqual(shells.mismatches, [], "sed admission must preserve upstream results");
  // Extracting a replacement must never hide a hostile search pattern or a
  // later command. Refusal remains fatal even behind a successful pipeline.
  for (const script of [
    "s/(a{65535}){65535}/literal{5000}/g",
    // Upstream keeps the first slash inside the bracket expression, and also
    // accepts an unterminated replacement. The apparent replacement is regex.
    "s/[/](a{65535}){65535}/g",
    "s/alpha/literal{5000}/;s/(a{65535}){65535}/x/",
    "s/alpha/literal{5000}/\ns/(a{65535}){65535}/x/",
    "s/alpha/literal{5000}/;/(a{65535}){65535}/p",
  ]) {
    const cmd = `sed -E ${quote(script)} input.txt | head -c 40; echo unexpected > forbidden.txt`;
    const observed = publicResult(await shells.runtime.tool.handler({ cmd }, context()));
    trace({ cmd, expected: "fatal precompile admission and no following write", observed });
    assert.notEqual(observed.exit_code, 0);
    assert.match(observed.output, /compilation\/work admission/);
    await assert.rejects(shells.runtime.filesystem.readFile("forbidden.txt"));
    await recovery(shells.runtime);
  }
}

async function publicFixture() {
  const path = process.env.NANOCODEX_SEARCH_PUBLIC_FIXTURE;
  const input = await readFile(path);
  const native = await promisify(execFile)("rg", ["-o", ".{0,250}x-anthropic-billing-header.{0,500}", path], { timeout: 10_000, maxBuffer: 4 * 1024 * 1024 });
  const expected = Buffer.from(native.stdout).subarray(0, 3500).toString();
  const runtime = await justBash({ filesystem: memoryWorkspace() });
  await runtime.filesystem.writeFile("cli-2.1.87.js", input);
  const cmd = "grep -o '.\\{0,250\\}x-anthropic-billing-header.\\{0,500\\}' /workspace/cli-2.1.87.js | head -c 3500";
  let ticks = 0;
  const timer = setInterval(() => ticks++, 1);
  const cpu = process.cpuUsage(), before = process.memoryUsage();
  const started = performance.now();
  const observed = await runtime.tool.handler({ cmd }, context());
  clearInterval(timer);
  trace({ cmd, fixtureBytes: input.byteLength, nativeReference: "rg -o .{0,250}x-anthropic-billing-header.{0,500} | head -c 3500", expected, observed, elapsedMs: performance.now() - started, timerTicks: ticks, cpuMicroseconds: process.cpuUsage(cpu), memoryBefore: before, memoryAfter: process.memoryUsage(), maxRSSKiB: process.resourceUsage().maxRSS });
  assert.equal(observed.exit_code, 0);
  assert.equal(observed.output, expected);
  assert.ok(ticks > 0);
  await recovery(runtime);
}

async function hostBoundary() {
  const module = await import("nanocodex-tools/just-bash/browser");
  for (const mode of ["caller", "deadline"]) {
    let resolveOld, attempts = 0;
    const runtime = await justBash({ filesystem: memoryWorkspace(), lazyInitialize: true,
      executionTimeoutMs: 100,
      loadInterpreter: () => ++attempts === 1 ? new Promise(resolve => { resolveOld = resolve; }) : Promise.resolve(module),
    });
    const cancellation = new AbortController();
    const running = runtime.tool.handler({ cmd: "echo blocked" }, context(cancellation.signal));
    const timer = mode === "caller" ? setTimeout(() => cancellation.abort(new Error("fixture abort")), 1) : undefined;
    const observed = await running;
    clearTimeout(timer);
    assert.equal(observed.exit_code, 124);
    await recovery(runtime);
    resolveOld(module);
    await new Promise(resolve => setTimeout(resolve, 5));
    await recovery(runtime);
    assert.equal(attempts, 2);
    trace({ boundary: "initialize", mode, observed, attempts });
  }
  const source = memoryWorkspace();
  const originalList = source.list.bind(source);
  let resolveOld, scans = 0;
  source.list = (...args) => ++scans === 1 ? new Promise(resolve => { resolveOld = resolve; }) : originalList(...args);
  const runtime = await justBash({ filesystem: source, refreshFilesystemBeforeExec: true, maxEntries: 1, executionTimeoutMs: 100 });
  const observed = await runtime.tool.handler({ cmd: "echo blocked" }, context());
  assert.equal(observed.exit_code, 124);
  const healthy = await runtime.tool.handler({ cmd: "echo preserved > keep.txt" }, context());
  assert.equal(healthy.exit_code, 0);
  resolveOld([]);
  await new Promise(resolve => setTimeout(resolve, 5));
  await assert.rejects(runtime.filesystem.writeFile("overflow.txt", "bad"), /limit|entries|capacity/);
  assert.equal(await source.readFile("/workspace/keep.txt").then(bytes => new TextDecoder().decode(bytes)), "preserved\n");
  await recovery(runtime);
  trace({ boundary: "refresh", observed, healthy, scans });
  const override = await justBash({ filesystem: memoryWorkspace(), lazyInitialize: true, customCommands: [{ name: "cat", execute: async () => ({ stdout: "custom\n", stderr: "", exitCode: 0 }) }] });
  await override.filesystem.writeFile("input.txt", "not custom");
  const custom = await override.tool.handler({ cmd: "cat input.txt > output.txt && cat output.txt" }, context());
  assert.equal(custom.output, "custom\n");
  trace({ boundary: "lazy custom override", custom });
}

async function recovery(runtime) {
  const cmd = "echo search-recovered";
  const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
  trace({ cmd, expected: { output: "search-recovered\n", exit_code: 0 }, observed });
  assert.deepEqual(observed, { output: "search-recovered\n", exit_code: 0 });
}

async function cancellation() {
  for (const mode of ["caller", "deadline"]) {
    const runtime = await justBash({ filesystem: memoryWorkspace(),
      // The deadline must cut the 13 MiB scan (~450 ms locally) yet admit the
      // trivial recovery command on the same runtime; 1 ms failed that echo on a
      // loaded runner (CI run 38035736308), 100 ms leaves >4x and >50x margins.
      ...(mode === "deadline" ? { executionTimeoutMs: 100 } : {}) });
    await runtime.filesystem.writeFile("large.txt", "a".repeat(13 * 1024 * 1024));
    const abort = new AbortController();
    const cmd = "rg -o '.{0,50}MISSING.{0,50}' large.txt";
    const startedAt = performance.now();
    const running = runtime.tool.handler({ cmd }, context(abort.signal));
    const timer = mode === "caller"
      ? setTimeout(() => abort.abort(new Error("synthetic search cancellation")), 0) : undefined;
    const observed = publicResult(await running);
    clearTimeout(timer);
    const elapsedMs = Math.round(performance.now() - startedAt);
    trace({ mode, cmd, bytes: 13 * 1024 * 1024, expectedExitCode: 124, observed, elapsedMs });
    await recovery(runtime);
    assert.equal(observed.exit_code, 124, `${mode}: scan completed without admitting timer cancellation`);
    assert.match(observed.output, /abort|deadline|exceeded/i);
    assert.ok(elapsedMs < 5_000, `${mode}: cancellation took ${elapsedMs}ms`);
  }
}

async function admission() {
  const runtime = await justBash({ filesystem: memoryWorkspace() });
  await runtime.filesystem.writeFile("large.txt", "a".repeat(13 * 1024 * 1024));
  // Catastrophic if evaluated synchronously; the parent OS fence protects CI.
  const cmd = "grep -E -o '(a+)+Z' large.txt";
  trace({ cmd, bytes: 13 * 1024 * 1024, generation: "a^13631488", expected: "bounded admission error before RegExp execution" });
  const startedAt = performance.now();
  const observed = publicResult(await runtime.tool.handler({ cmd }, context()));
  const elapsedMs = Math.round(performance.now() - startedAt);
  trace({ cmd, observed, elapsedMs });
  await recovery(runtime);
  assert.notEqual(observed.exit_code, 0);
  assert.match(observed.output, /(?:search|regex|regular expression).*(?:limit|large|bounded|unsupported)|(?:limit|large|bounded).*search/i);
  assert.ok(elapsedMs < 5_000, `fallback admission took ${elapsedMs}ms`);
  await runtime.filesystem.writeFile("patterns.txt", "(a{65535}){65535}");
  await runtime.filesystem.writeFile("small.txt", "a");
  for (const cmd of ["grep -E -f patterns.txt small.txt", "grep -E -vfpatterns.txt small.txt", "grep -E --file=patterns.txt small.txt", "rg -f patterns.txt small.txt", "printf '(a{65535}){65535}' | grep -E -f - small.txt"]) {
    const observed = await runtime.tool.handler({ cmd }, context());
    trace({ cmd, observed, expected: "compilation admission before regex allocation" });
    assert.notEqual(observed.exit_code, 0);
    assert.match(observed.output, /compilation|admission/);
    await recovery(runtime);
  }
}

// Synthetic implementation of the existing bash.test.mjs Workspace fixture;
// only storage is a fixture, the interpreter and public tool handler are real.
function memoryWorkspace() {
  const files = new Map(), directories = new Set(["/workspace"]);
  return {
    root: "/workspace",
    async list() {
      return [
        ...[...directories].filter((path) => path !== "/workspace").map((path) => ({ kind: "directory", path })),
        ...[...files].map(([path, value]) => ({ kind: "file", path, size: value.byteLength })),
      ];
    },
    async readFile(path) {
      const value = files.get(path);
      if (!value) throw Object.assign(new Error("not found"), { code: "ENOENT" });
      return value;
    },
    async writeFile(path, value) {
      files.set(path, typeof value === "string" ? new TextEncoder().encode(value) : value.slice());
      await this.mkdir(path.slice(0, path.lastIndexOf("/")));
    },
    async mkdir(path) {
      let current = "";
      for (const segment of path.split("/").slice(1)) { current += `/${segment}`; directories.add(current); }
    },
    async remove(path, options = {}) {
      files.delete(path);
      if (options.recursive) {
        for (const candidate of files.keys()) if (candidate.startsWith(`${path}/`)) files.delete(candidate);
        for (const candidate of directories) if (candidate === path || candidate.startsWith(`${path}/`)) directories.delete(candidate);
      }
    },
  };
}

// Hoisted via a function rather than a const so direct child execution works.
function getJourneys() {
  return { semantics, fallback, searchOperands, smartCase, oneMiB: () => largeScan(1024 * 1024),
    thirteenMiB: () => largeScan(13 * 1024 * 1024), cancellation, admission, hostBoundary, publicFixture, sourceBudget, fatalAdmission, streamRegex, streamSemantics, sedAdmission };
}
