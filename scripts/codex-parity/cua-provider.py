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
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
runtime = root / 'crates/experimental/nanocodex-computer/runtime/src'
provenance = json.loads((runtime / 'cua_provider_provenance.json').read_text())
for source in provenance['sources']:
    actual = hashlib.sha256((args.reference_root / source['path']).read_bytes()).hexdigest()
    assert actual == source['sha256'], f"Provider source changed: {source['path']}"
for name, version in provenance['packages'].items():
    assert json.loads((args.reference_root / name / 'package.json').read_text())['version'] == version
catalog = json.loads((runtime / 'cua_provider_tools.json').read_text())
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
print(f"Verified {len(provenance['sources'])} provider source hashes and all platform descriptions")
