#!/usr/bin/env python3
"""Regenerate/check shared tool contract evidence against a pinned upstream checkout.

Usage: python3 scripts/codex-parity/check.py /path/to/openai/codex [--write]
Compiles the actual upstream schema constructors with their actual JsonSchema type,
using a minimal serialization-only wrapper (no Codex runtime or credentials).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import shutil
import tempfile

PIN = '36430b36881cf5c289cb48e671cfc9e8b542ae7b'
ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / 'crates/nanocodex-tools/tests/fixtures/codex-parity/shared-tools.json'

def raw(source, name):
    match = re.search(r'const ' + name + r': &str = r(#*)"(.*?)"\1;', source, re.S)
    assert match, name
    return match[2]

def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('upstream', type=Path)
parser.add_argument('--write', action='store_true')
args = parser.parse_args()
upstream = args.upstream.resolve()
assert run('git', '-C', str(upstream), 'rev-parse', 'HEAD') == PIN, 'wrong upstream revision'
assert not run('git', '-C', str(upstream), 'status', '--porcelain', '--untracked-files=no'), 'upstream tracked edits'
u = upstream / 'codex-rs'
protocol = (u / 'code-mode-protocol/src/description.rs').read_text()
wait = raw(protocol, 'WAIT_DESCRIPTION_TEMPLATE')
paths = {
    'schema': 'tools/src/json_schema.rs',
    'shell': 'core/src/tools/handlers/shell_spec.rs',
    'plan': 'core/src/tools/handlers/plan_spec.rs',
    'view': 'core/src/tools/handlers/view_image_spec.rs',
    'wait': 'core/src/tools/code_mode/wait_spec.rs',
    'description': 'code-mode-protocol/src/description.rs',
    'json_schema_types': 'code-mode-protocol/src/json_schema_types.rs',
}
# These wrappers only serialize the upstream constructors' return values. All
# schema construction, options, and descriptions below come from upstream files.
source = '''#![allow(dead_code, unused_imports)]
extern crate self as codex_tools;
extern crate self as codex_protocol;
extern crate self as codex_code_mode;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolName { pub namespace: Option<String>, pub name: String }

pub use schema::JsonSchema;
pub mod models { pub const VIEW_IMAGE_TOOL_NAME: &str = "view_image"; }
pub const PUBLIC_TOOL_NAME: &str = "exec";
pub const WAIT_TOOL_NAME: &str = "wait";
#[derive(Serialize)]
pub struct ResponsesApiTool {
    name: String, description: String, strict: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    defer_loading: Option<bool>,
    parameters: JsonSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_schema: Option<serde_json::Value>,
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolSpec { Function(ResponsesApiTool) }
'''
source += 'pub fn build_wait_tool_description() -> String { ' + json.dumps(wait) + '.to_owned() }\n'
for name, path in paths.items():
    source += "mod schema;\n" if name == "schema" else f'#[path = {json.dumps(str(u / path))}] mod {name};\n'
image_source = (u / 'ext/image-generation/src/tool.rs').read_text()
image_args = image_source[image_source.index('#[derive(Debug, Deserialize, JsonSchema)]'):image_source.index('fn legacy_end_event')]
source += 'mod imagegen { use serde::Deserialize; use schemars::{JsonSchema, r#gen::SchemaSettings}; use codex_utils_absolute_path::AbsolutePathBuf;\n' + image_args + '''
    pub fn parameters() -> super::JsonSchema {
        let value = serde_json::to_value(SchemaSettings::draft2019_09()
            .with(|s| s.inline_subschemas = true).into_generator().into_root_schema_for::<ImagegenArgs>()).unwrap();
        let mut input = serde_json::Map::new();
        for key in ["properties", "required", "type", "additionalProperties"] {
            if let Some(value) = value.get(key) { input.insert(key.to_owned(), value.clone()); }
        }
        super::schema::parse_tool_input_schema(&serde_json::Value::Object(input)).unwrap()
    }
}
'''
source += '''fn main() {
    let tools = [
        shell::create_exec_command_tool_with_environment_id(shell::CommandToolOptions {
            allow_login_shell: true, exec_permission_approvals_enabled: false,
        }, false, true, false),
        shell::create_write_stdin_tool(), plan::create_update_plan_tool(),
        view::create_view_image_tool(view::ViewImageToolOptions {
            can_request_original_image_detail: true, unified_image_budget: false,
            include_environment_id: false,
        }), wait::create_wait_tool(),
        ToolSpec::Function(ResponsesApiTool { name: "image_gen__imagegen".into(), description: include_str!("IMAGE_DESCRIPTION_PATH").into(),
            strict: false, defer_loading: None, parameters: imagegen::parameters(), output_schema: None }),
    ];
    let definitions = tools.iter().map(|ToolSpec::Function(tool)| description::ToolDefinition {
        name: tool.name.clone(), tool_name: ToolName { namespace: None, name: tool.name.clone() },
        description: tool.description.clone(), kind: description::CodeModeToolKind::Function,
        input_schema: Some(serde_json::to_value(&tool.parameters).unwrap()), output_schema: tool.output_schema.clone(),
    }).collect::<Vec<_>>();
    let wrappers = definitions.iter().cloned().map(|tool| {
        let augmented = description::augment_tool_definition(tool);
        (augmented.name, augmented.description)
    }).collect::<std::collections::BTreeMap<_, _>>();
    println!("{}", serde_json::json!({"tools": tools, "wrappers": wrappers}));
}
'''
source = source.replace('IMAGE_DESCRIPTION_PATH', str(u / 'ext/image-generation/imagegen_description.md'))
with tempfile.TemporaryDirectory(prefix='codex-parity-') as tmp:
    tmp = Path(tmp)
    (tmp / 'src').mkdir()
    (tmp / 'Cargo.toml').write_text('''[package]
name = "codex-parity-extract"
version = "0.0.0"
edition = "2024"
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "=0.8.22"
urlencoding = "2"
jsonptr = "0.6"
''' + 'codex-utils-absolute-path = { path = ' + json.dumps(str(u / 'utils/absolute-path')) + ' }\n')
    shutil.copyfile(u / 'tools/src/json_schema.rs', tmp / 'src/schema.rs')
    shutil.copytree(u / 'tools/src/json_schema', tmp / 'src/schema')
    (tmp / 'src/main.rs').write_text(source)
    extracted = json.loads(run('cargo', 'run', '--quiet', '--manifest-path', str(tmp / 'Cargo.toml'),
                          env={**os.environ, 'CARGO_TARGET_DIR': str(ROOT / 'target/codex-parity')}))

fixture = {
    'upstream': PIN,
    'configuration': 'non-Windows; login/shell enabled; approval extension/environment_id disabled; original image detail enabled; unified image budget disabled',
    'sources': {p: hashlib.sha256((u / p).read_bytes()).hexdigest() for p in paths.values()},
    'tools': {tool['name']: tool for tool in extracted['tools']},
    'wrappers': extracted['wrappers'],
    'exec_description': raw(protocol, 'EXEC_DESCRIPTION_TEMPLATE'),
    'exec_grammar': raw((u / 'core/src/tools/code_mode/execute_spec.rs').read_text(), 'CODE_MODE_FREEFORM_GRAMMAR'),
    'mcp_preamble': raw(protocol, 'MCP_TYPESCRIPT_PREAMBLE'),
}
if args.write:
    FIXTURE.write_text(json.dumps(fixture, indent=2) + '\n')
else:
    assert json.loads(FIXTURE.read_text()) == fixture, 'upstream fixture drift'

for local, remote in [
    ('crates/nanocodex-tools/src/apply_patch/apply_patch.lark', 'core/assets/tools/apply_patch.lark'),
    ('crates/nanocodex-tools/src/image_generation/imagegen_description.md', 'ext/image-generation/imagegen_description.md'),
]:
    assert (ROOT / local).read_bytes() == (u / remote).read_bytes(), local
print(f'PASS: upstream {PIN}; compiled tool constructors, patch grammar, imagegen description')

# The renderer is a direct port: permit only import/module-path adaptation.
renderer = (ROOT / 'crates/nanocodex-tools/src/code_mode/schema_types.rs').read_text()
renderer = renderer.removeprefix(f'// Ported from openai/codex {PIN}.\n')
renderer = renderer.replace('use super::normalize_identifier as normalize_code_mode_identifier;',
                            'use crate::description::normalize_code_mode_identifier;')
renderer = renderer.replace('schema_types_tests.rs', 'json_schema_types_tests.rs')
assert renderer == (u / 'code-mode-protocol/src/json_schema_types.rs').read_text(), 'Code Mode schema renderer drift'
print('PASS: exact upstream Code Mode schema renderer (imports/module path adapted)')

