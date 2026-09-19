#!/usr/bin/env python3
"""Compile pinned upstream constructors into the shared native/JS catalog.

No constructor is handwritten. The extractor compiles upstream schema sources or
verbatim function bodies in a serialization-only harness. Runtime providers stay
explicit in source_inventory; publishing a contract never registers a handler.
"""
import argparse, hashlib, itertools, json, os, re, shutil, subprocess, tempfile
from pathlib import Path
PIN = '36430b36881cf5c289cb48e671cfc9e8b542ae7b'
ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'crates/nanocodex-tools/src/catalog/upstream.json'
JS = ROOT / 'js/nanocodex-tools/tools/codexCatalog.generated.mjs'
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('upstream', type=Path)
p.add_argument('--write', action='store_true')
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
module('shell', 'core/src/tools/handlers/shell_spec.rs')
for login, approvals, env, shell, windows in itertools.product([False,True], repeat=5):
    flags = [login,approvals,env,shell,windows]
    values = ','.join(str(x).lower() for x in flags[2:])
    key = 'exec_command/'+''.join(str(int(x)) for x in flags)
    add(key, f'shell::create_exec_command_tool_with_environment_id(shell::CommandToolOptions{{allow_login_shell:{str(login).lower()},exec_permission_approvals_enabled:{str(approvals).lower()}}},{values})', dict(zip(['login','approvals','environment_id','shell','windows'], flags)))
add('write_stdin/default','shell::create_write_stdin_tool()')
add('request_permissions/default','shell::create_request_permissions_tool(shell::request_permissions_tool_description())')
for name, funcs in {
 'plan':['update_plan'], 'mcp_resource':['list_mcp_resources','list_mcp_resource_templates','read_mcp_resource'],
 'get_context_remaining':['get_context_remaining'], 'new_context_window':['new_context_window'],
 'list_available_plugins_to_install':['list_available_plugins_to_install'], 'test_sync':['test_sync'],
}.items():
    module(name, f'core/src/tools/handlers/{name}_spec.rs')
    for f in funcs: add(f+'/default',f'{name}::create_{f}_tool()')
module('goal','ext/goal/src/spec.rs')
for f in ['get_goal','create_goal','update_goal']: add(f+'/default',f'goal::create_{f}_tool()')
module('view','core/src/tools/handlers/view_image_spec.rs')
for original,budget,env in itertools.product([False,True],repeat=3):
    flags=[original,budget,env]
    add('view_image/'+''.join(str(int(x)) for x in flags),f'view::create_view_image_tool(view::ViewImageToolOptions{{can_request_original_image_detail:{str(original).lower()},unified_image_budget:{str(budget).lower()},include_environment_id:{str(env).lower()}}})',dict(zip(['original_detail','unified_image_budget','environment_id'],flags)))
module('patch','core/src/tools/handlers/apply_patch_spec.rs')
for env in [False,True]: add('apply_patch/'+str(int(env)),f'patch::create_apply_patch_freeform_tool({str(env).lower()})',{'environment_id':env})
module('install','core/src/tools/handlers/request_plugin_install_spec.rs')
for presentation in ['ListTool','RecommendationContext']: add('request_plugin_install/'+presentation,f'install::create_request_plugin_install_tool(tools::router::ToolSuggestPresentation::{presentation})')
# Compile the actual UTF-8 boundary implementation used for source descriptions.
string_source=read('utils/string/src/lib.rs')
header += function(string_source,'take_bytes_at_char_boundary')+'\n'
module('search','core/src/tools/handlers/tool_search_spec.rs')
for listing in ['Include','Omit']:
    add('tool_search/'+listing,f'search::create_tool_search_tool(&[],8,search::ToolSearchSourceListing::{listing})',{'default_limit':8,'sources':[],'source_listing':listing})
# Runtime-dependent methods: extract their exact body and inject their actual default inputs.
for name in ['current_time','sleep','request_user_input_async','send_message_to_user_async','wait_for_environment']:
    path=f'core/src/tools/handlers/{name}.rs'; text=read(path)
    if name=='sleep': code=constants(text)+'\n'+function(text,'create_sleep_tool').replace('fn create_sleep_tool','pub fn create_sleep_tool'); expr=f'{name}::create_sleep_tool()'
    else:
        fn=function(text,'spec',True).replace('fn spec(&self)', 'pub fn spec(description: String, environment_id_description: String)')
        fn=fn.replace('self.description.clone()','description.clone()').replace('self.tool_description.clone()','description.clone()').replace('self.environment_id_description.clone()','environment_id_description.clone()')
        code=constants(text)+'\n'+fn
        if name=='wait_for_environment': code+='\npub fn default_spec()->ToolSpec {spec(DEFAULT_TOOL_DESCRIPTION.into(),DEFAULT_ENVIRONMENT_ID_DESCRIPTION.into())}'; expr=f'{name}::default_spec()'
        elif name=='request_user_input_async':
            messages=read('prompts/src/model_messages.rs')
            description=json.loads(re.search(r'const REQUEST_USER_INPUT_ASYNC_DESCRIPTION: &str = (".*?");',messages)[1])
            expr=f'{name}::spec({json.dumps(description)}.into(),String::new())'
        else: expr=f'{name}::spec(String::new(),String::new())'
    body_module(name,path,code); add(name+'/default',expr)
# Request-user-input constructor is independent of its runtime argument normalization.
text=read('core/src/tools/handlers/request_user_input_spec.rs')
body_module('input','', function(text,'create_request_user_input_tool')+'\n'+constants(text))
# Description generator uses ModeKind's display labels. Preserve its body and actual enum.
mode_source=read('protocol/src/config_types.rs')
mode_enum=re.search(r'pub enum ModeKind \{.*?^}',mode_source,re.S|re.M)[0]
mode_impl=re.search(r'impl ModeKind \{.*?^}',mode_source,re.S|re.M)[0]
mods.append('mod input_modes { use super::*; #[derive(Clone,Copy,PartialEq,Eq,Default,Serialize,Deserialize)] '+mode_enum+'\n'+mode_impl+'\n'+function(text,'request_user_input_tool_description')+'\n'+function(text,'format_allowed_modes')+'}')
for mode in ['Plan','Default']:
    add('request_user_input/'+mode,f'input::create_request_user_input_tool(input_modes::request_user_input_tool_description(&[input_modes::ModeKind::{mode}]))')
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
# Track every source that constructs a tool, including providers requiring live data.
provider_files=[]
for directory in ['core/src/tools','ext','tools/src']:
    for path in sorted((u/directory).rglob('*.rs')):
        if any(x in path.parts for x in ['tests','snapshots']) or path.stem.endswith('_tests'): continue
        text=path.read_text()
        if re.search(r'(fn (?:spec|create_\w*tool)\b|ToolSpec::(?:Function|Freeform|Namespace|ToolSearch|WebSearch)\s*\{)',text):
            provider_files.append(str(path.relative_to(u)))
            read(str(path.relative_to(u)))
source=header+'\n'+'\n'.join(mods)+'\nfn main(){let mut out=std::collections::BTreeMap::new();\n'+'\n'.join(calls)+'\nprintln!("{}",serde_json::to_string(&out).unwrap());}\n'
with tempfile.TemporaryDirectory(prefix='codex-catalog-') as tmp:
    tmp=Path(tmp); (tmp/'src').mkdir()
    (tmp/'Cargo.toml').write_text('''[package]
name="codex-catalog-extract"
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
catalog={'upstream':PIN,'variants':variants,'source_inventory':provider_files,'sources':sources}
serialized=json.dumps(catalog,indent=2,sort_keys=True)+'\n'
js='// Generated by scripts/codex-parity/generate_catalog.py; do not edit.\nexport default '+serialized.rstrip()+';\n'
for path,content in [(DEST,serialized),(JS,js)]:
    if a.write: path.parent.mkdir(parents=True,exist_ok=True); path.write_text(content)
    else: assert path.read_text()==content,f'catalog drift: {path}'
print(f'PASS: {len(variants)} variants; {len(provider_files)} provider sources; {PIN}')
