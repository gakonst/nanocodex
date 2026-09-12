// Independent formatter for retained declarative API/documentation data.
((manifest, index, documents, info, knownCapabilities, options={}) => {
 const disabled=new Set(options.disabledMemberIds??[]);
 for(const [type,members] of Object.entries(manifest.interfaces))for(const [name,spec] of Object.entries(members))if((info.apiSupportOverrides?.[type+'.'+name]??!spec.unsupportedByDefaultIn?.includes(info.type))===false)disabled.add(type+'.'+name);
 for(const name of options.undocumentedApiMembers??['Tab.ax'])disabled.add(name);
 const ids=scope=>new Set((info.capabilities?.[scope]??[]).map(capability=>capability.id));
 const browserIds=ids('browser'),tabIds=ids('tab');
 const selected=index.filter(doc=>{const when=doc.when??{};return (when.browserTypes===undefined||when.browserTypes.includes(info.type))&&!when.requiredApiMembers?.some(name=>disabled.has(name))&&!when.requiredBrowserCapabilities?.some(id=>!browserIds.has(id))&&!when.requiredTabCapabilities?.some(id=>!tabIds.has(id))&&!when.excludedTabCapabilities?.some(id=>tabIds.has(id));});
 const text=['# Selected Browser','- Name: '+info.name,'- Type: '+info.type,'- ID: '+info.id,'Reuse this browser binding across later turns. A new user turn or tab error does not invalidate it; select another browser only when the browser-selection policy requires it.','If a tab is stale or missing later, obtain or create a fresh tab from this browser; never reselect a browser to recover a tab. Empty tab lists are normal after cleanup and do not invalidate this browser binding.'].join('\n');
 const sections=[text,...selected.filter(doc=>doc.mode==='included'&&!options.excludedDocumentation?.includes(doc.name)).map(doc=>documents[doc.name])];
 const lookup=selected.filter(doc=>doc.mode==='lookup');if(lookup.length)sections.push(['# Additional Documentation','Use `await agent.documentation.get("<name>")` when you need one of these topics:',...lookup.map(doc=>'- `'+doc.name+'`: '+doc.description)].join('\n'));
 const capabilities=['# Additional Capabilities'];for(const scope of ['browser','tab']){capabilities.push('## '+(scope==='browser'?'Browser':'Tab')+' Capabilities');const rows=(info.capabilities?.[scope]??[]).filter(cap=>knownCapabilities[scope]?.[cap.id]).map(cap=>'- `'+cap.id+'`: '+knownCapabilities[scope][cap.id].description+'\n  Read with `await (await '+scope+'.capabilities.get("'+cap.id+'")).documentation()`.');capabilities.push(...(rows.length?rows:['- None']));}sections.push(capabilities.join('\n'));
 const declarations=name=>Object.entries(manifest.interfaces[name]).flatMap(([member,spec])=>disabled.has(name+'.'+member)||spec.documented===false?[]:spec.declarations.filter(declaration=>declaration.documented!==false&&!declaration.unsupportedByDefaultIn?.includes(info.type)));
 const queue=[manifest.root],reachable=new Set();for(let next=0;next<queue.length;next++){const name=queue[next];if(reachable.has(name))continue;const refs=manifest.interfaces[name]?declarations(name).flatMap(d=>d.references):manifest.types[name]?.references;if(refs===undefined)continue;reachable.add(name);queue.push(...refs.filter(ref=>!reachable.has(ref)));}
 const lines=['# API Reference','','Use this as the supported `agent.browsers.*` surface.','','```ts','// Returned by setupBrowserRuntime().','// browser was selected during bootstrap.'];
 for(const name of Object.keys(manifest.interfaces))if(reachable.has(name))lines.push('interface '+name+' {',...declarations(name).map(d=>'  '+d.text),'}','');
 for(const [name,spec] of Object.entries(manifest.types))if(reachable.has(name))lines.push(spec.text,'');
 lines[lines.length-1]='```';sections.push(lines.join('\n').trim()+'\n');return sections.join('\n\n');
})
