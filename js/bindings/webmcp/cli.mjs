#!/usr/bin/env node
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

import { generate, validate } from "./generator.mjs";

const [command = "generate", ...argv] = process.argv.slice(2);

try {
  if (command === "generate") await generateCommand(argv);
  else if (command === "check") await checkCommand(argv);
  else if (command === "help" || command === "--help" || command === "-h") help();
  else throw new Error(`unknown command: ${command}`);
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}

async function generateCommand(args) {
  let root = ".";
  let output = "webmcp.manifest.json";
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--out" || argument === "-o") {
      output = requiredValue(args[++index], argument);
    } else if (argument === "--help" || argument === "-h") {
      help();
      return;
    } else if (argument.startsWith("-")) {
      throw new Error(`unknown option: ${argument}`);
    } else if (root === ".") root = argument;
    else throw new Error(`unexpected argument: ${argument}`);
  }
  const manifest = await generate({ root });
  validate(manifest);
  const path = resolve(output);
  await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`, { flag: "w" });
  process.stdout.write(`Generated ${manifest.tools.length} review-required WebMCP tools in ${path}\n`);
}

async function checkCommand(args) {
  if (args.length !== 1) throw new Error("usage: nanocodex-webmcp check <manifest.json>");
  const path = resolve(args[0]);
  const manifest = JSON.parse(await readFile(path, "utf8"));
  validate(manifest);
  const approved = manifest.tools.filter((tool) => tool.approved === true).length;
  process.stdout.write(`Valid WebMCP manifest: ${manifest.tools.length} tools, ${approved} approved\n`);
}

function requiredValue(value, option) {
  if (typeof value !== "string" || !value || value.startsWith("-")) {
    throw new Error(`${option} requires a value`);
  }
  return value;
}

function help() {
  process.stdout.write(`nanocodex-webmcp

  generate [repository] [--out webmcp.manifest.json]
      Analyze source as data and write inert draft tools for review.

  check <manifest.json>
      Validate a generated or edited manifest.

Generated tools are never approved or published automatically.
`);
}
