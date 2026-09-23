// Source fingerprints, independent of build outputs and checkout location.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { dirname, posix } from 'node:path';
import { fingerprint as fingerprintWasm } from '../../js/nanocodex-vite/scripts/wasm-output-cache.mjs';

export const workerSpecs = Object.fromEntries([
  ['egress', 'js/egress', 'nanocodex-egress-service', true],
  ['x', 'js/x-api', '@nanocodex/x-api', false],
  ['managed', 'js/managed', 'nanocodex-managed-service', true],
  ['email', 'js/email', 'nanocodex-email-service', false],
  ['dialog', 'js/connect-dialog', '@nanocodex/connect-dialog', true],
  ['connect-api', 'js/connect-api', '@nanocodex/connect-api', true],
  ['astra', 'examples/astra-mpp-trial', 'nanocodex-astra-mpp-trial', true],
  ['chief-of-staff', 'js/chief-of-staff', '@nanocodex/chief-of-staff', true],
  ['playground', 'js/connect-playground', '@nanocodex/connect-playground', true],
  ['account', 'js/account', 'nanocodex-web', true],
].map(([name, directory, pkg, needsWasm]) => [name, { directory, package: pkg, needsWasm }]));

const buildTargets = {
  egress: ['nanocodex'], x: ['nanocodex-tools'],
  managed: ['nanocodex', 'nanocodex-connect-protocol'], email: [],
  dialog: ['@nanocodex/connect-dialog'], 'connect-api': ['nanocodex', '@nanocodex/connect-api'],
  astra: ['nanocodex'], 'chief-of-staff': ['nanocodex'],
  playground: ['@nanocodex/connect-playground'], account: ['nanocodex-web'],
};
for (const [name, targets] of Object.entries(buildTargets)) workerSpecs[name].buildTargets = targets;

const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const generated = /(?:^|\/)(?:node_modules|dist|target|pkg-web|pkg-node|\.wrangler|\.turbo|\.git)(?:\/|$)/;
const tests = /(?:^|\/)(?:test|tests|benchmark)(?:\/|$)|\.(?:test|spec)\.[^/]+$/;
const common = /^(?:package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|turbo\.json|\.npmrc|\.node-version|\.nvmrc|tsconfig[^/]*\.json|patches\/.*|scripts\/cloudflare\/(?:worker-inputs|release-plan|release-workers)\.mjs|\.github\/workflows\/cloudflare\.yml|\.github\/actions\/wasm-outputs\/action\.yml)$/;

export async function fingerprintWorkers(cwd = process.cwd()) {
  // Include new source files but never ignored/generated local build products.
  const paths = [...new Set(execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], { cwd, encoding: 'utf8' }).split('\0').filter(Boolean))]
    .filter(path => !generated.test(path)).sort();
  const contents = new Map();
  for (const path of paths) {
    try { contents.set(path, await readFile(`${cwd}/${path}`)); }
    catch (error) { if (error.code !== 'ENOENT') throw error; } // Deleted working-tree inputs.
  }
  const packages = new Map();
  for (const [path, bytes] of contents) {
    if (/^(?:js|examples)\/[^/]+\/package\.json$/.test(path)) {
      const manifest = JSON.parse(bytes);
      packages.set(manifest.name, { directory: dirname(path), manifest });
    }
  }
  let wasm;
  const result = {};
  for (const [name, spec] of Object.entries(workerSpecs)) {
    const files = new Set([...contents.keys()].filter(path => common.test(path)));
    const visited = new Set();
    function addDirectory(directory) {
      for (const path of contents.keys()) if (path.startsWith(`${directory}/`) && !tests.test(path)) files.add(path);
    }
    function visit(pkg) {
      if (visited.has(pkg)) return;
      visited.add(pkg);
      const entry = packages.get(pkg);
      if (!entry) throw new Error(`Missing local package: ${pkg}`);
      addDirectory(entry.directory);
      const dependencies = { ...entry.manifest.dependencies, ...entry.manifest.devDependencies, ...entry.manifest.optionalDependencies, ...entry.manifest.peerDependencies };
      for (const [dependency, version] of Object.entries(dependencies)) {
        if (packages.has(dependency)) visit(dependency);
        else if (/^(workspace:|file:|link:)/.test(version)) throw new Error(`Unresolved local dependency: ${dependency}`);
      }
    }
    visit(spec.package);
    // Follow relative imports/re-exports and literal build asset URLs without
    // treating a development-only Wrangler service binding as a dependency.
    // Imported sibling files bring only their own transitive file references.
    for (const path of files) {
      if (!/\.(?:[cm]?[jt]sx?|jsonc?)$/.test(path)) continue;
      const source = contents.get(path).toString();
      const references = /(?:\bfrom\s*|\bimport\s*(?:\(\s*)?|\brequire\s*\(\s*|\bnew\s+URL\s*\(\s*)["'](\.[^"'\n]+)["']/g;
      for (const match of source.matchAll(references)) {
        const target = posix.normalize(posix.join(dirname(path), match[1].split(/[?#]/)[0])).replace(/\/$/, '');
        if (target === '.' || target.startsWith('../')) continue;
        for (const candidate of [target, ...['.ts', '.tsx', '.js', '.mjs', '/index.ts', '/index.tsx', '/index.js', '/index.mjs'].map(ext => target + ext)]) {
          if (contents.has(candidate)) files.add(candidate);
        }
        // Directory assets are build inputs, but repository/package-root URLs
        // used to locate the checkout must not pull in unrelated Workers.
        if (match[1].endsWith('/') && !path.startsWith(`${target}/`)) addDirectory(target);
      }
    }
    const needsWasm = visited.has('nanocodex');
    if (needsWasm) wasm ??= await fingerprintWasm(cwd, 'release');
    result[name] = digest(JSON.stringify({ schema: 1, spec, wasm: needsWasm ? wasm : null,
      files: [...files].sort().map(path => [path, digest(contents.get(path))]) }));
  }
  return result;
}
