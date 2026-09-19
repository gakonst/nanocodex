#!/usr/bin/env python3
"""Verify the pinned CUA provider assets against an installed @oai package root.

Reads source files and contracts only; never starts a provider or captures a screen.
"""
import argparse
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--reference-root', type=Path, required=True,
                    help='Directory containing installed cua, cua-repl and sky packages')
parser.add_argument('--catalog', type=Path,
                    help='Optional freshly captured MCP tools/list result to compare in full')
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
runtime = root / 'crates/experimental/nanocodex-computer/runtime/src'
provenance = json.loads((runtime / 'cua_provider_provenance.json').read_text())
for source in provenance['sources']:
    actual = hashlib.sha256((args.reference_root / source['path']).read_bytes()).hexdigest()
    assert actual == source['sha256'], f"Provider source changed: {source['path']}"
for name, version in provenance['packages'].items():
    assert json.loads((args.reference_root / name / 'package.json').read_text())['version'] == version
node_repl = args.reference_root.parents[2] / 'bin/node_repl'
assert hashlib.sha256(node_repl.read_bytes()).hexdigest() == provenance['node_repl_sha256']
catalog = json.loads((runtime / 'cua_provider_tools.json').read_text())
if args.catalog:
    assert catalog == json.loads(args.catalog.read_text()), 'Live MCP tool catalog changed'
js = next(t for t in catalog['tools'] if t['name'] == 'js')
reset = next(t for t in catalog['tools'] if t['name'] == 'js_reset')
instructions = args.reference_root / 'cua-repl/instructions'
for platform in ('macos', 'linux', 'windows'):
    expected = '\n\n'.join((instructions / platform / f'{name}.md').read_text().rstrip()
                            for name in ('description', 'browser', 'computer', 'output'))
    assert js['description'] == expected, f'{platform} tool description changed'
assert reset['description'] == (instructions / 'reset.md').read_text().rstrip()
assert js['inputSchema']['properties']['code']['description'] == (instructions / 'code.md').read_text().rstrip()
assert (runtime / 'cua_tool_description.md').read_text() == js['description']
assert (runtime / 'cua_reset_description.md').read_text() == reset['description']
assert (runtime.parent.parent / 'src/description.md').read_text() == js['description']
assert (runtime.parent.parent / 'src/reset_description.md').read_text() == reset['description']
types = 'cua/dist/lib/js/oai_js_cua/src/tinysky_alt/types.d.ts'
assert (runtime / 'cua_provider_types.d.ts').read_bytes() == (args.reference_root / types).read_bytes()
node_root = args.reference_root.parents[2]
assert hashlib.sha256((node_root / 'bin/node').read_bytes()).hexdigest() == provenance['node_sha256']
assert json.loads((node_root / 'manifest.json').read_text())['node_version'] == provenance['node_version']
# The generated JS is a fixed wrapper around a JSON object; inspect it as data.
docs = (runtime / 'cua_docs.js').read_text()
prefix = ('// Pinned installed CUA provider documentation; see cua_provider_provenance.json.\n'
          'Object.defineProperty(globalThis, "__skyreDocumentation", {value:Object.freeze(')
suffix = '), configurable:false});\n'
assert docs.startswith(prefix) and docs.endswith(suffix), 'Unexpected documentation wrapper'
documents = json.loads(docs[len(prefix):-len(suffix)])
for name, text in documents.items():
    assert text == (args.reference_root / 'cua/docs' / f'tinysky-alt-{name}.md').read_text(), name
assert set(documents) == {'confirmations', 'core-cua-repl', 'core-node-repl', 'other-browser-apis'}
print(f"Verified {len(provenance['sources'])} provider source hashes, executable hash, "
      "types, documentation and all platform descriptions")
