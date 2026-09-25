// Source fingerprints, independent of build outputs and checkout location.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { dirname, posix } from 'node:path';
import { fingerprintInputs as fingerprintWasm } from '../../js/nanocodex-vite/scripts/wasm-output-cache.mjs';

export const workerSpecs = Object.fromEntries([
  ['egress', 'js/egress', 'nanocodex-egress-service', false],
  ['x', 'js/x-api', '@nanocodex/x-api', false],
  ['media', 'js/media', 'nanocodex-media-service', false],
  ['managed', 'js/managed', 'nanocodex-managed-service', true],
  ['email', 'js/email', 'nanocodex-email-service', false],
  ['dialog', 'js/connect-dialog', '@nanocodex/connect-dialog', false],
  ['connect-api', 'js/connect-api', '@nanocodex/connect-api', false],
  ['astra', 'examples/astra-mpp-trial', 'nanocodex-astra-mpp-trial', false],
  ['chief-of-staff', 'js/chief-of-staff', '@nanocodex/chief-of-staff', false],
  ['playground', 'js/connect-playground', '@nanocodex/connect-playground', true],
  ['account', 'js/account', 'nanocodex-web', true],
].map(([name, directory, pkg, needsWasm]) => [name, { directory, package: pkg, needsWasm }]));

const buildTargets = {
  egress: ['nanocodex-tools'], x: ['nanocodex-tools'], media: ['nanocodex-tools'],
  managed: ['nanocodex-tools', 'nanocodex-connect-protocol', 'nanocodex'], email: [],
  dialog: ['nanocodex-connect-protocol', 'nanocodex-connect-ui', '@nanocodex/connect-dialog'],
  'connect-api': ['nanocodex-tools', 'nanocodex-connect-protocol', '@nanocodex/connect-api'],
  astra: ['nanocodex-tools'], 'chief-of-staff': ['nanocodex-tools'],
  playground: ['nanocodex-tools', 'nanocodex', 'nanocodex-terminal', '@nanocodex/connect-playground'],
  account: ['nanocodex-tools', 'nanocodex-connect-protocol', 'nanocodex', 'nanocodex-connect-ui', 'nanocodex-terminal', 'nanocodex-web'],
};

for (const [name, targets] of Object.entries(buildTargets)) workerSpecs[name].buildTargets = targets;

const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const generated = /(?:^|\/)(?:node_modules|dist|target|pkg-web|pkg-node|\.wrangler|\.turbo|\.git)(?:\/|$)/;
const tests = /(?:^|\/)(?:test|tests|benchmark|benches)(?:\/|$)|\.(?:test|spec)\.[^/]+$/;
const common = /^(?:package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|turbo\.json|\.npmrc|\.node-version|\.nvmrc|tsconfig[^/]*\.json|patches\/.*|scripts\/cloudflare\/(?:worker-inputs|release-plan|release-workers)\.mjs|\.github\/workflows\/cloudflare\.yml|\.github\/actions\/(?:wasm-outputs|deploy-workers)\/action\.yml)$/;

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
    function addDirectory(directory, packageSource = false) {
      for (const path of contents.keys()) {
        if (!path.startsWith(`${directory}/`) || tests.test(path)) continue;
        const relative = path.slice(directory.length + 1);
        if (packageSource && ((directory === 'js/account' && relative.startsWith('container/'))
          || /^(?:Dockerfile(?:\..*)?|\.dockerignore)$/.test(relative)
          || /^(?:scripts|\.github)\//.test(relative)
          || /^(?:README|CHANGELOG|AGENTS)\.md$/.test(relative)
          || (directory === 'js/nanocodex' && /^(?:src\/|Cargo\.toml$)/.test(relative)))) continue;
        files.add(path);
      }
    }
    function addBuildScripts(entry) {
      const seen = new Set();
      const visitScript = name => {
        if (seen.has(name)) return;
        seen.add(name);
        const command = entry.manifest.scripts?.[name] ?? '';
        for (const match of command.matchAll(/(?:^|[\s;])(?:\.\/)?(scripts\/[^\s;&|]+\.(?:[cm]?js|sh|py))(?=$|[\s;])/g)) {
          const path = `${entry.directory}/${match[1]}`;
          if (contents.has(path)) files.add(path);
        }
        for (const match of command.matchAll(/(?:npm|pnpm)\s+run\s+([\w:-]+)/g)) visitScript(match[1]);
      };
      for (const script of ['prebuild', 'build', 'postbuild']) visitScript(script);
    }
    function visit(pkg) {
      if (visited.has(pkg)) return;
      visited.add(pkg);
      const entry = packages.get(pkg);
      if (!entry) throw new Error(`Missing local package: ${pkg}`);
      addDirectory(entry.directory, true);
      // Only scripts used by the selected build can contribute runtime assets.
      if (spec.buildTargets.includes(pkg)) addBuildScripts(entry);
      const dependencies = { ...entry.manifest.dependencies, ...entry.manifest.devDependencies, ...entry.manifest.optionalDependencies, ...entry.manifest.peerDependencies };
      for (const [dependency, version] of Object.entries(dependencies)) {
        if (packages.has(dependency)) visit(dependency);
        else if (/^(workspace:|file:|link:)/.test(version)) throw new Error(`Unresolved local dependency: ${dependency}`);
      }
    }
    visit(spec.package);
    if (name === 'managed') files.add('js/managed/scripts/prepare-code-evaluator.mjs');
    if (name === 'account') files.add('scripts/cloudflare/released-account-image.mjs');
    if (name === 'managed') {
      files.add('scripts/cloudflare/released-images.mjs');
      files.add('scripts/cloudflare/managed-crm.mjs');
    }
    for (const path of files) if (!contents.has(path)) files.delete(path);
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
    const needsWasm = spec.needsWasm;
    if (needsWasm) wasm ??= await fingerprintWasm(cwd, 'release');
    result[name] = digest(JSON.stringify({ schema: 2, spec, wasm: needsWasm ? wasm : null,
      files: [...files].sort().map(path => [path, digest(contents.get(path))]) }));
  }
  return result;
}
