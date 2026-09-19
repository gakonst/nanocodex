#!/usr/bin/env python3
"""Verify the four consumed memory tool declarations against pinned Codex constructors."""
import argparse, hashlib, itertools, json, os, re, shutil, subprocess, tempfile
from pathlib import Path
PIN = '36430b36881cf5c289cb48e671cfc9e8b542ae7b'
ROOT = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('upstream', type=Path)
a = p.parse_args()
u = a.upstream.resolve() / 'codex-rs'
def run(*args, **kw): return subprocess.check_output(args, text=True, **kw).strip()
assert run('git', '-C', str(u.parent), 'rev-parse', 'HEAD') == PIN
assert not run('git', '-C', str(u.parent), 'status', '--porcelain', '--untracked-files=no')
sources = {}
def read(path):
    text = (u/path).read_text()
    sources[path] = hashlib.sha256(text.encode()).hexdigest()
    return text

def function(text, name, method=False):
    # Rust items terminate at their own indentation, never at nested braces.
    indent = '    ' if method else ''
    match = re.search(r'^'+indent+r'(?:pub(?:\([^)]*\))? )?fn '+name+r'\b.*?^'+indent+r'}', text, re.S|re.M)
    assert match, name
    return match[0]

def constants(text):
    return '\n'.join(re.findall(r'^(?:pub(?:\([^)]*\))? )?const .*?;(?=\n)', text, re.S|re.M))

header = r'''#![allow(dead_code, unused_imports)]
extern crate self as codex_tools;
extern crate self as codex_protocol;
extern crate self as codex_utils_string;
use serde::{Deserialize, Serialize};
pub use schema::JsonSchema;
pub use schema::parse_tool_input_schema_without_compaction;
#[derive(Clone, Debug, Serialize)]
pub struct ResponsesApiTool {
 name: String, description: String, strict: bool,
 #[serde(skip_serializing_if="Option::is_none")] defer_loading: Option<bool>,
 parameters: JsonSchema,
 #[serde(skip_serializing_if="Option::is_none")] output_schema: Option<serde_json::Value>,
}
#[derive(Clone, Debug, Serialize)] pub struct FreeformTool {name:String, description:String, #[serde(skip_serializing_if="Option::is_none")] defer_loading:Option<bool>, format:FreeformToolFormat}
#[derive(Clone, Debug, Serialize)] pub struct FreeformToolFormat {r#type:String,syntax:String,definition:String}
#[derive(Clone, Debug, Serialize)] pub struct ResponsesApiNamespace {name:String,description:String,tools:Vec<ResponsesApiNamespaceTool>}
#[derive(Clone, Debug, Serialize)] #[serde(tag="type",rename_all="snake_case")] pub enum ResponsesApiNamespaceTool {Function(ResponsesApiTool), Custom(FreeformTool)}
#[derive(Clone, Debug, Serialize)] #[serde(tag="type")] pub enum ToolSpec {
 #[serde(rename="function")] Function(ResponsesApiTool),
 #[serde(rename="custom")] Freeform(FreeformTool),
 #[serde(rename="namespace")] Namespace(ResponsesApiNamespace),
 #[serde(rename="tool_search")] ToolSearch{execution:String,description:String,parameters:JsonSchema},
}
pub const DEFAULT_FUNCTION_NAMESPACE: &str="functions";
pub const TOOL_SEARCH_TOOL_NAME: &str="tool_search";
pub const LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME: &str="list_available_plugins_to_install";
pub const REQUEST_PLUGIN_INSTALL_TOOL_NAME: &str="request_plugin_install";
pub struct ToolSearchSourceInfo {pub name:String,pub description:Option<String>}
pub mod models {pub const VIEW_IMAGE_TOOL_NAME:&str="view_image";}
pub mod tools {pub mod router {pub enum ToolSuggestPresentation {ListTool,RecommendationContext}}}
mod schema;
'''
mods = []
calls = []
def module(name, path):
    read(path)
    mods.append(f'#[path={json.dumps(str(u/path))}] mod {name};')
def body_module(name, path, code):
    mods.append('mod '+name+' { use super::*; use std::collections::BTreeMap; use serde_json::{json,Value};\n'+code+'\n}')
def add(key, expression, config=None):
    calls.append(f'out.insert({json.dumps(key)}.to_string(), serde_json::json!({{"spec":{expression},"configuration":{json.dumps(config or {})}}}));')
# Memory API: compile the actual input/output types and namespace constructor.
header += function(read('tools/src/responses_api.rs'), 'default_namespace_description')+'\n'
header += 'pub use schema::parse_tool_input_schema;\n'
memory_code = 'use schemars::JsonSchema;\n'+constants(read('ext/memories/src/lib.rs'))+'\n'
for name,path in [('backend','ext/memories/src/backend.rs'), ('schema','ext/memories/src/schema.rs')]:
    read(path)
    memory_code += '#[path='+json.dumps(str(u/path))+'] mod '+name+';\n'
memory_code += 'use backend::*;\n'
memory_code += function(read('ext/memories/src/tools/mod.rs'),'memory_function_tool')+'\n'
for name in ['list','read','search','ad_hoc_note']:
    source_text=read('ext/memories/src/tools/'+name+'.rs')
    args=re.search(r'#\[derive\([^\n]*Deserialize[^\n]*\)\]\n#\[serde\(deny_unknown_fields\)\]\nstruct .*?^}',source_text,re.S|re.M)
    assert args,name
    memory_code += args[0]+'\n'
    memory_code += function(source_text,'spec',True).replace('fn spec(&self)', 'pub fn '+name+'()')+'\n'
    add('memories/'+name,'memories::'+name+'()')
body_module('memories','',memory_code)
source=header+'\n'+'\n'.join(mods)+'\nfn main(){let mut out=std::collections::BTreeMap::new();\n'+'\n'.join(calls)+'\nprintln!("{}",serde_json::to_string(&out).unwrap());}\n'
with tempfile.TemporaryDirectory(prefix='codex-memory-') as tmp:
    tmp=Path(tmp); (tmp/'src').mkdir()
    (tmp/'Cargo.toml').write_text('''[package]
name="codex-memory-check"
version="0.0.0"
edition="2024"
[dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
urlencoding="2"
jsonptr="0.6"
schemars="0.8"
thiserror="2"
''')
    shutil.copyfile(u/'tools/src/json_schema.rs',tmp/'src/schema.rs')
    shutil.copytree(u/'tools/src/json_schema',tmp/'src/schema')
    (tmp/'src/main.rs').write_text(source)
    variants=json.loads(run('cargo','run','--quiet','--manifest-path',str(tmp/'Cargo.toml'),env={**os.environ,'CARGO_TARGET_DIR':str(ROOT/'target/codex-parity')}))
actual=[]
for entry in variants.values():
    namespace=entry['spec']
    tool=namespace['tools'][0]
    actual.append({'name': namespace['name']+'__'+tool['name'], 'description':tool['description'], 'parameters':tool['parameters'], 'outputSchema':tool['output_schema']})
expected=json.loads((ROOT/'js/nanocodex-tools/tools/extension-specs.json').read_text())
assert expected == json.loads((ROOT/'crates/nanocodex-tools/src/extensions/specs.json').read_text()), 'native/JS memory declaration drift'
assert sorted(actual,key=lambda s:s['name'])==sorted(expected,key=lambda s:s['name']), 'memory tool schema drift'
print('PASS: four memory declarations equal pinned upstream constructors '+PIN)
