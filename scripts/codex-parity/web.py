#!/usr/bin/env python3
"""Extract the exact pinned Codex web schema and description for Rust and JS."""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
PIN = '36430b36881cf5c289cb48e671cfc9e8b542ae7b'
ROOT = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('upstream', type=Path)
p.add_argument('--write', action='store_true')
a = p.parse_args()
u = a.upstream.resolve()
assert subprocess.check_output(['git', '-C', str(u), 'rev-parse', 'HEAD'], text=True).strip() == PIN
assert not subprocess.check_output(['git', '-C', str(u), 'status', '--porcelain', '--untracked-files=no'], text=True).strip()
src = (u/'codex-rs/codex-api/src/search.rs').read_text()
start = src.index('#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, JsonSchema)]')
end = src.index('#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]\n#[serde(rename_all = "snake_case")]')
commands = src[start:end]
local_wire = (ROOT/'crates/nanocodex-tools/src/web_search/wire.rs').read_text()
assert commands.replace('pub ', 'pub(super) ') in local_wire, 'web command wire drift'
schema = (u/'codex-rs/ext/web-search/src/schema.rs').read_text().replace('use codex_api::SearchCommands;', '')
with tempfile.TemporaryDirectory(prefix='codex-web-') as directory:
    directory = Path(directory)
    (directory/'src').mkdir()
    (directory/'Cargo.toml').write_text('[package]\nname="codex-web-parity"\nversion="0.0.0"\nedition="2024"\n[dependencies]\nserde={version="1",features=["derive"]}\nserde_json="1"\nschemars="0.8"\n')
    (directory/'src/main.rs').write_text('use serde::{Serialize,Deserialize};\nuse schemars::JsonSchema;\n'+commands+schema+'\nfn main(){println!("{}",commands_schema());}\n')
    schema_json = json.loads(subprocess.check_output(['cargo','run','--quiet','--manifest-path',str(directory/'Cargo.toml')], text=True, env={**os.environ, 'CARGO_TARGET_DIR': str(ROOT/'target/codex-parity')}))
description = (u/'codex-rs/ext/web-search/web_run_description.md').read_text()
outputs = {
 ROOT/'crates/nanocodex-tools/tests/fixtures/codex-parity/web.json': json.dumps(schema_json, indent=2, sort_keys=True)+'\n',
 ROOT/'js/nanocodex-tools/tools/webParameters.generated.mjs': '// Generated from pinned codex-rs by scripts/codex-parity/web.py.\nexport default '+json.dumps(schema_json, indent=2, sort_keys=True)+';\n',
 ROOT/'crates/nanocodex-tools/src/web_search/web_run_description.md': description,
}
for path, expected in outputs.items():
    if a.write: path.write_text(expected)
    else: assert path.read_text() == expected, f'web contract drift: {path}'
path = ROOT/'js/nanocodex-tools/tools/standardDescriptions.mjs'
lines = path.read_text().splitlines(keepends=True)
line = 'export const WEB_DESCRIPTION = '+json.dumps(description, ensure_ascii=False)+';\n'
if a.write: path.write_text(''.join(line if x.startswith('export const WEB_DESCRIPTION = ') else x for x in lines))
else: assert line in lines, 'JS web description drift'
print('PASS: pinned web wire, schema and exact description for native and JS')
