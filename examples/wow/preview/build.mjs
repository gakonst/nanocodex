import {copyFile, mkdir, writeFile, readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
await mkdir('public/lua',{recursive:true});
const hashes={};
for(const name of ['Context','Core','Projects','Bridge','Client']){
 const source=`../addon/Nanocodex/${name}.lua`;
 await copyFile(source,`public/lua/${name}.lua`);
 hashes[name]=createHash('sha256').update(await readFile(source)).digest('hex');
}
await copyFile('wow-api.lua','public/wow-api.lua');
await copyFile('node_modules/fengari-web/dist/fengari-web.js','public/fengari-web.js');
await copyFile('node_modules/fengari-web/LICENSE','public/FENGARI-LICENSE');
await writeFile('public/lua-source-hashes.json',JSON.stringify(hashes,null,2)+'\n');
const result=await Bun.build({entrypoints:['app.js'],target:'browser',minify:true,outdir:'public'});
if(!result.success)throw new AggregateError(result.logs,'Browser build failed');
