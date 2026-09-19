// node --experimental-vm-modules tests/cua_current_facade.mjs [installed cua directory]
// Loads only factory/get_state/helpers; all provider imports and docs are inert.
import fs from 'node:fs/promises';
import path from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const installed = process.argv.slice(2).find(value=>!value.startsWith('--')) ?? '/Applications/ChatGPT.app/Contents/Resources/cua_node/lib/node_modules/@oai/cua';
const sourceRoot = path.join(installed,'dist/lib/js/oai_js_cua/src');
const cases = await fs.readFile(path.join(here,'cua_current_facade_cases.js'),'utf8');
async function capture(reference, script = cases) {
  const context = vm.createContext({URL,TextEncoder,Uint8Array,Buffer,console});
  if(reference) {
    const modules = new Map();
    const synthetic = (names,values) => new vm.SyntheticModule(names,function(){names.forEach((name,i)=>this.setExport(name,values[i]));},{context});
    const doc = synthetic(['read_documentation','read_computer_use_confirmation_policy'],[
      name=>context.config.readDocumentation(name),()=>context.config.readDocumentation('confirmations')]);
    async function load(filename) {
      if(modules.has(filename))return modules.get(filename);
      const module = new vm.SourceTextModule(await fs.readFile(filename,'utf8'),{context,identifier:filename,
        importModuleDynamically:async specifier=>{
          const module = specifier.endsWith('browser-client.js') ? synthetic(['setupBrowserRuntime'],[()=>({browsers:context.config.browsers})])
            : specifier.endsWith('sky_js/src/index.js') ? synthetic(['sky'],[context.config.computer])
            : (()=>{throw Error('Unexpected dynamic import: '+specifier);})();
          await module.link(()=>{throw Error('Unexpected synthetic dependency');});await module.evaluate();return module;
        }});
      modules.set(filename,module);
      await module.link((specifier,importer)=>specifier==='./documentation.js'?doc:load(path.resolve(path.dirname(importer.identifier),specifier)));
      return module;
    }
    const module = await load(path.join(sourceRoot,'tinysky_alt/create_tinysky_alt.js'));
    await module.evaluate();
    context.__testCreateCUA = config => {context.config=config;return module.namespace.create_tinysky_alt({browser:config.browsers!==undefined,computer:config.computer!==undefined});};
  } else {
    vm.runInContext(await fs.readFile(path.join(here,'../src/facade.js'),'utf8'),context);
    context.__testCreateCUA = context.__skyreCreateCUA;
  }
  return JSON.parse(JSON.stringify(await vm.runInContext(script,context)));
}
const reference = await capture(true);
const actual = await capture(false);
assert.deepEqual(actual,reference);
if(process.argv.includes('--write-oracle'))await fs.writeFile(path.join(here,'oracles/cua_current_facade.json'),JSON.stringify(reference,null,2)+'\n');
else assert.deepEqual(JSON.parse(await fs.readFile(path.join(here,'oracles/cua_current_facade.json'),'utf8')),reference,'Regenerate current facade oracle using --write-oracle');
console.log('Installed CUA factory and compatibility facade match all inert scenarios.');

// Current globals eagerly creates cua without exporting setupCUA. The following
// cases compose the retained pre-bootstrap compatibility seam with the actual
// installed factory; they do not claim this seam is an installed public API.
const legacyPrelude = `
const fixtureConfig = {
 computer:{target:'mac'}, browsers:{},
 readDocumentation:async()=>'', getNodeRepl:()=>undefined
};
globalThis.cua={async initialize(){}};
let legacySetup;
function __legacySetupCUA(options={}) {
 return legacySetup ??= __testCreateCUA({...fixtureConfig,
  computer:options.computer===false?undefined:fixtureConfig.computer,
  browsers:options.browser===false?undefined:fixtureConfig.browsers
 }).then(api=>{Object.assign(cua,api,{initialize:api.getState});});
}
`;
const sourceHashes = {};
for (const file of ['tinysky_alt/create_tinysky_alt.js','tinysky_alt/globals.js','get_state.js']) {
 sourceHashes[file] = createHash('sha256').update(await fs.readFile(path.join(sourceRoot,file))).digest('hex');
}
for (const [kind,variable] of [['publication','publicationCase'],['assignment','assignmentCase']]) {
 const oraclePath=path.join(here,`oracles/cua_setup_${kind}_cases.json`);
 const previous=JSON.parse(await fs.readFile(oraclePath,'utf8'));
 const body=(await fs.readFile(path.join(here,`cua_setup_${kind}_case.js`),'utf8'))
  .replace("var {setupCUA} = await import('@oai/cua/tinyskyAlt');",'var setupCUA=__legacySetupCUA;')
  .replace(/\n\(\{initial:/,'\nreturn ({initial:');
 const captured=[];
 for (const {id} of previous.cases) {
  const script=`(async()=>{${legacyPrelude} var ${variable}=${JSON.stringify(id)}; ${body}})()`;
  const expected=await capture(true,script);
  const actual=await capture(false,script);
  assert.deepEqual(actual,expected,`${kind}: ${id}`);
  captured.push({id,expected});
 }
 const oracle={schema:2,scope:'Current installed CUA factory composed with the retained pre-bootstrap compatibility setup seam; inert providers only. The installed globals module exports no setupCUA. Engine-specific nonextensible Error.message is excluded by the Rust comparison.',sourceHashes,cases:captured};
 if(process.argv.includes('--write-oracle'))await fs.writeFile(oraclePath,JSON.stringify(oracle,null,2)+'\n');
 else assert.deepEqual(previous,oracle,`Regenerate ${kind} oracle using --write-oracle`);
}
console.log('Current factory compatibility setup seam matches 11 inert publication/assignment cases.');
