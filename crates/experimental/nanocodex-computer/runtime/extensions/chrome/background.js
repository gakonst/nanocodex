// Independent MV3 adapter. Only the private native host supplies ownership context.
importScripts('leases.js');
const leases=new SkyreLeaseStore(chrome),sessions=new Map(),frameTargets=new Map();let port,delay=1000;
let chunkId=0,pendingChunk;
const expectedDownloads=new Map(),activeDownloads=new Map();
const nativeAssetRequests=new Map();let nativeAssetId=0;
function tabContextAsset(operation,params){
 if(!['create','appendChunk','finish','remove','abort'].includes(operation))return Promise.reject(new Error('Invalid tab context asset operation'));
 if(!port)return Promise.reject(new Error('Native asset channel is disconnected'));
 if(nativeAssetRequests.size>=16)return Promise.reject(new Error('Too many pending native asset requests'));
 const requestPort=port,id='skyre-asset-'+(++nativeAssetId);
 return new Promise((resolve,reject)=>{
  const timer=setTimeout(()=>{if(nativeAssetRequests.delete(id))reject(new Error('Native asset request timed out'));},30000);
  nativeAssetRequests.set(id,{port:requestPort,resolve,reject,timer});
  if(!send({jsonrpc:'2.0',id,method:'codexRuntime/tabContextAsset/'+operation,params})){nativeAssetRequests.delete(id);clearTimeout(timer);reject(new Error('Native asset channel could not send request'));}
 });
}
function nativeAssetResponse(value,requestPort){
 if(typeof value.id!=='string'||!value.id.startsWith('skyre-asset-'))return false;
 const pending=nativeAssetRequests.get(value.id);
 if(!pending||pending.port!==requestPort)return true;
 nativeAssetRequests.delete(value.id);clearTimeout(pending.timer);
 if(value.error){const error=new Error(String(value.error.message));error.code=value.error.code;pending.reject(error);}
 else if(Object.hasOwn(value,'result'))pending.resolve(value.result);
 else pending.reject(new Error('Invalid native asset response'));
 return true;
}
const send=value=>{const sendingPort=port;if(!sendingPort)return false;try{const bytes=new TextEncoder().encode(JSON.stringify(value));if(bytes.length<=512*1024){sendingPort.postMessage(value);return true;}const id=++chunkId,total=Math.ceil(bytes.length/(256*1024));for(let part=0;part<total;part++){const chunk=bytes.subarray(part*256*1024,(part+1)*256*1024);let text='';for(let i=0;i<chunk.length;i+=32768)text+=String.fromCharCode(...chunk.subarray(i,i+32768));sendingPort.postMessage({__skyre_chunk:{id,part,total,data:btoa(text)}});}return true;}catch{disconnectPort(sendingPort);return false;}};
function receive(value){if(!value.__skyre_chunk){if(pendingChunk)throw new Error('Interleaved chunks');return value;}const c=value.__skyre_chunk;if(!Number.isSafeInteger(c.id)||!Number.isInteger(c.part)||!Number.isSafeInteger(c.total)||c.total<1)throw new Error('Invalid chunk');if(!pendingChunk){if(c.part!==0)throw new Error('Missing chunk');pendingChunk={id:c.id,total:c.total,part:0,bytes:[],length:0};}const p=pendingChunk;if(p.id!==c.id||p.total!==c.total||p.part!==c.part)throw new Error('Stale chunk');const bytes=Uint8Array.from(atob(c.data),c=>c.charCodeAt(0));if(bytes.length>256*1024)throw new Error('Invalid chunk size');p.bytes.push(bytes);p.length+=bytes.length;p.part++;if(p.part<p.total)return undefined;const out=new Uint8Array(p.length);let offset=0;for(const bytes of p.bytes){out.set(bytes,offset);offset+=bytes.length;}pendingChunk=undefined;return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(out));}
function forgetTab(id){for(const[key,value]of expectedDownloads)if(value.tabId===id)expectedDownloads.delete(key);for(const[key,value]of activeDownloads)if(value.tabId===id)activeDownloads.delete(key);for(const[key,s]of sessions)if(s.tabId===id)sessions.delete(key);for(const[key,f]of frameTargets)if(f.tabId===id)frameTargets.delete(key);}
async function detachTab(id){if(sessions.has('tab-'+id)){try{await chrome.debugger.detach({tabId:id});}catch{}finally{forgetTab(id);}}}
async function detachAll(){for(const id of new Set([...sessions.values()].map(s=>s.tabId)))await detachTab(id);}
function retirePort(connectedPort){
 for(const[id,pending]of nativeAssetRequests)if(pending.port===connectedPort){nativeAssetRequests.delete(id);clearTimeout(pending.timer);pending.reject(new Error('Native asset channel disconnected'));}
 if(port!==connectedPort)return;
 port=undefined;pendingChunk=undefined;leases.serial(detachAll).catch(()=>{});setTimeout(connect,delay);delay=Math.min(delay*2,60000);
}
function disconnectPort(connectedPort=port){
 if(!connectedPort)return;
 // Chrome fires onDisconnect only at the other end of a local disconnect.
 // https://developer.chrome.com/docs/extensions/reference/api/runtime#type-Port
 retirePort(connectedPort);
 try{connectedPort.disconnect();}catch{}
}
function connect(){
 if(port)return;
 try{
  const connectedPort=chrome.runtime.connectNative('org.nanocodex.computer');port=connectedPort;delay=1000;
  connectedPort.onMessage.addListener(value=>{
   if(port!==connectedPort)return;
   let request;try{request=receive(value);}catch{disconnectPort(connectedPort);return;}
   if(!request||nativeAssetResponse(request,connectedPort))return;
   dispatch(request).then(result=>{if(port===connectedPort)send({id:request.id,result});},error=>{if(port===connectedPort)send({id:request.id,error:{code:Number.isInteger(error.code)?error.code:-32000,message:String(error.message??error)}});});
  });
  connectedPort.onDisconnect.addListener(()=>{
   void chrome.runtime.lastError;
   retirePort(connectedPort);
  });
 }catch{port=undefined;setTimeout(connect,delay);delay=Math.min(delay*2,60000);}
}
chrome.action.onClicked.addListener(connect);chrome.runtime.onStartup.addListener(connect);chrome.runtime.onInstalled.addListener(connect);
function tabId(value){const text=String(value);const id=Number(text);if(!/^\d+$/.test(text)||!Number.isSafeInteger(id))throw new Error('Invalid tab ID');return id;}
const info=tab=>({targetId:String(tab.id),type:'page',title:tab.title??'',url:tab.url??'',attached:sessions.has('tab-'+tab.id),browserContextId:String(tab.windowId)});
function requireSession(context,id){const session=sessions.get(id);if(!session)throw new Error('Debugger session detached');leases.require(context,session.tabId);return session;}
function supportedUrl(url){let parsed;try{parsed=new URL(url);}catch{throw new Error('Invalid tab URL');}if(!['http:','https:','file:','data:','about:'].includes(parsed.protocol)||parsed.protocol==='about:'&&parsed.href!=='about:blank')throw new Error('Internal browser pages cannot be controlled');return url;}
async function badge(id,text){await chrome.action.setBadgeText({tabId:id,text});}
async function ownsGroup(context,id){
 try{await chrome.tabGroups.get(id);}catch{return false;}
 const members=(await chrome.tabs.query({})).filter(tab=>tab.groupId===id);
 return members.length>0&&members.every(tab=>{try{return leases.require(context,tab.id).origin==='agent';}catch{return false;}});
}
async function pruneGroups(context){const state=leases.current(context),groups={};for(const[window,id]of Object.entries(state.groups??{}))if(await ownsGroup(context,id))groups[window]=id;
 await leases.transaction(({sessions})=>{sessions.get(context.sessionId).groups=groups;});return groups;
}
async function groupTab(context,id){const state=leases.current(context),tab=await chrome.tabs.get(id);let groupId=(await pruneGroups(context))[tab.windowId];
 groupId=await chrome.tabs.group({tabIds:[id],...(groupId===undefined?{}:{groupId})});await chrome.tabGroups.update(groupId,{title:state.name??'Nanocodex',color:'blue'});await leases.transaction(({sessions})=>{const state=sessions.get(context.sessionId);state.groups??={};state.groups[tab.windowId]=groupId;});}
async function applyViewport(context,id){const value=await leases.takeViewport(context,id);if(value===undefined)return;if(value===null)await chrome.debugger.sendCommand({tabId:id},'Emulation.clearDeviceMetricsOverride',{});else await chrome.debugger.sendCommand({tabId:id},'Emulation.setDeviceMetricsOverride',{...value,mobile:value.mobile??false,deviceScaleFactor:value.deviceScaleFactor??1});}
async function attach(context,id){leases.require(context,id);const key='tab-'+id;if(sessions.has(key))return{sessionId:key};let recovered=false;try{await chrome.debugger.attach({tabId:id},'1.3');}catch(error){try{await chrome.debugger.sendCommand({tabId:id},'Runtime.enable',{});recovered=true;}catch{throw error;}}sessions.set(key,{tabId:id});try{if(recovered)await chrome.debugger.sendCommand({tabId:id},'Target.setAutoAttach',{autoAttach:false,waitForDebuggerOnStart:false,flatten:true});await chrome.debugger.sendCommand({tabId:id},'Target.setAutoAttach',{autoAttach:true,waitForDebuggerOnStart:false,flatten:true,filter:[{type:'iframe',exclude:false},{exclude:true}]});await applyViewport(context,id);await badge(id,'CUA');}catch(error){await detachTab(id);throw error;}return{sessionId:key};}
async function finish(context){
 await leases.transaction(({sessions})=>{delete sessions.get(context.sessionId).pendingViewport;});
 const tabs=await leases.list(context),owned=tabs.map(tab=>({tab,lease:leases.require(context,tab.id)}));
 // Preserve the original effect ordering. Failure keeps the turn active for explicit retry.
 for(const {tab,lease}of owned)try{await badge(tab.id,lease.mark==='handoff'?'WAIT':lease.mark==='deliverable'?'DONE':'');}catch{}
 for(const {tab}of owned)await detachTab(tab.id);
 const state=leases.current(context),groups=new Set(Object.values(state.groups??{}));
 for(const {tab,lease}of owned)if(lease.mark==='deliverable'&&groups.has(tab.groupId))await chrome.tabs.ungroup(tab.id);
 for(const {tab,lease}of owned)if(lease.origin==='agent'&&!lease.mark){await chrome.tabs.remove(tab.id);forgetTab(tab.id);}
 const live=new Set((await chrome.tabs.query({})).map(tab=>tab.id));await leases.finish(context,live);
 return {ended:true};
}
const pendingCommand=Symbol('pending debugger command');
function debuggerCommand(context,sessionId,debuggee,method,params){
 const lease={...leases.require(context,debuggee.tabId)};
 // Send while ownership is serialized, but never hold that queue across a CDP
 // acknowledgement: Page.navigate may require a later Fetch continuation.
 const promise=Promise.resolve(chrome.debugger.sendCommand(debuggee,method,params)).then(value=>({value:value??{}}),error=>({error}));
 return{[pendingCommand]:{promise,validate(){const current=leases.require(context,debuggee.tabId);if(sessions.get(sessionId)!==debuggee||current.claimedAt!==lease.claimedAt||current.instanceId!==lease.instanceId)throw new Error('Debugger command ownership changed before acknowledgement');}}};
}
async function dispatch(request){const result=await leases.serial(()=>dispatchOwned(request)),pending=result?.[pendingCommand];if(!pending)return result;const response=await pending.promise;pending.validate();if(Object.hasOwn(response,'error'))throw response.error;return response.value;}
async function dispatchOwned(request){
 if(!request||!Number.isSafeInteger(request.id)||typeof request.method!=='string')throw new Error('Invalid bridge request');
 const context=leases.context(request._skyreContext),p=request.params??{};
 if(request.method==='Skyre.hostLifecycle'){
  if(request._skyreHostAuthorized!==true)throw new Error('Trusted extension host lifecycle is required');
  const previous=leases.sessions.get(context.sessionId);
  if(p.phase==='started'){
   if(previous?.retired.includes(context.turnId))throw new Error('Browser turn is stale');
   if(previous?.turnId&&previous.turnId!==context.turnId)await finish({...context,turnId:previous.turnId});
   await leases.begin(context);return{started:true,sessionId:context.sessionId,turnId:context.turnId};
  }
  if(p.phase==='ended'){
   if(previous?.turnId===context.turnId)return finish(context);
   if(previous?.retired.includes(context.turnId))return{ended:true};
   throw new Error('Host lifecycle does not match an active extension turn');
  }
  throw new Error('Invalid trusted host lifecycle phase');
 }
 if(context.mode!=='current'&&context.mode!=='cached')throw new Error('Invalid browser context mode');
 if(context.mode==='current')await leases.begin(context);else leases.current(context);
 const method=request.method;
 if(method==='Page.setDownloadBehavior'){const error=new Error('Page-scoped download control changes browser-context authority and is unavailable through the extension');error.code=-32601;throw error;}
 if(method==='Skyre.beginTurn')return{sessionId:context.sessionId,turnId:context.turnId,instanceId:leases.instanceId};
 if(method==='Skyre.turnEnded')return finish(context);
 if(method==='Skyre.getInfo')return{type:'extension',instanceId:leases.instanceId,sessionId:context.sessionId,turnId:context.turnId};
 if(method==='Skyre.claimTab'){
  const id=tabId(p.tab??p.targetId),tab=await chrome.tabs.get(id),previous=leases.leases.get(id);
  if(previous?.state==='active'&&previous.sessionId!==context.sessionId)throw new Error('Tab is already owned by another browser session');
  const url=tab.pendingUrl??tab.url??'about:blank',newTab=/^(chrome|edge):\/\/newtab\/?$/.test(url);if(!newTab)supportedUrl(url);
  await leases.claim(context,id,'user');if(newTab)await chrome.tabs.update(id,{url:'about:blank'});await badge(id,'CUA');return{id:String(id),providerTabId:String(id)};
 }
 if(method==='Skyre.markTab'){const id=tabId(p.tab??p.targetId);await leases.mark(context,id,p.status);return{tab:String(id)};}
 if(method==='Skyre.nameSession'){if(typeof p.name!=='string'||p.name.length>256)throw new Error('Invalid browser session name');await leases.transaction(({sessions})=>{sessions.get(context.sessionId).name=p.name});for(const id of Object.values(await pruneGroups(context)))await chrome.tabGroups.update(id,{title:p.name});return{};}
 if(method==='Skyre.finalizeTabs'){
  if(!Array.isArray(p.keep))throw new Error('keep must be an array');const seen=new Set();for(const item of p.keep){const id=tabId(item.tabId);if(seen.has(id)||!['handoff','deliverable'].includes(item.status))throw new Error('Invalid kept tab');leases.require(context,id);seen.add(id);}
  await leases.transaction(({leases:tabs})=>{for(const lease of tabs.values())if(lease.sessionId===context.sessionId&&lease.turnId===context.turnId)delete lease.mark;for(const item of p.keep)tabs.get(tabId(item.tabId)).mark=item.status});return finish(context);
 }
 if(method==='Skyre.setViewport'){const id=p.tab===undefined?((await leases.list(context)).find(tab=>tab.active)?.id??null):tabId(p.tab);await leases.viewport(context,id,p.value??null);if(id!==null){if(sessions.has('tab-'+id))await applyViewport(context,id);else await attach(context,id);}return{};}
 if(method==='Skyre.openTabs'){const tabs=await chrome.tabs.query({});tabs.sort((a,b)=>(b.lastAccessed??0)-(a.lastAccessed??0));return{tabs:await Promise.all(tabs.map(async tab=>{let tabGroup;if(tab.groupId>=0)tabGroup=(await chrome.tabGroups.get(tab.groupId)).title;return{id:String(tab.id),providerTabId:String(tab.id),title:tab.title,url:tab.url,...(tab.lastAccessed===undefined?{}:{lastOpened:new Date(tab.lastAccessed).toISOString()}),...(tabGroup?{tabGroup}:{})};}))};}
 if(method==='Skyre.history'){
  const queries=p.queries??[p.text??''],limit=Math.min(p.limit??p.maxResults??100,1000),startTime=p.from===undefined?(p.startTime??0):Date.parse(p.from),endTime=p.to===undefined?(p.endTime??Date.now()):Date.parse(p.to);
  if(!Array.isArray(queries)||queries.some(q=>typeof q!=='string')||!Number.isInteger(limit)||limit<1||!Number.isFinite(startTime)||!Number.isFinite(endTime))throw new Error('Invalid history query');
  const rows=new Map();for(const text of queries)for(const item of await chrome.history.search({text,startTime,endTime,maxResults:limit}))if(!rows.has(item.url)||(rows.get(item.url).lastVisitTime??0)<(item.lastVisitTime??0))rows.set(item.url,item);
  return{items:[...rows.values()].sort((a,b)=>(b.lastVisitTime??0)-(a.lastVisitTime??0)).slice(0,limit).map(item=>({url:item.url,title:item.title,dateVisited:new Date(item.lastVisitTime??0).toISOString()}))};
 }
 if(request.sessionId){
  const debuggee=requireSession(context,request.sessionId);
  if(method==='Skyre.downloadExpect'){
   if(debuggee.sessionId)throw new Error('Child frame downloads cannot be attributed by the extension');
   if(typeof p.watchId!=='string'||!p.watchId.startsWith('download-watch-')||p.watchId.length>128||typeof p.url!=='string'||p.url.length>32768)throw new Error('Invalid owned download expectation');
   const url=new URL(p.url);if(!['http:','https:'].includes(url.protocol))throw new Error('Download URL must use HTTP(S)');
   if(expectedDownloads.size>=128||activeDownloads.size>=1024)throw new Error('Download ownership record limit exceeded');
   for(const expectation of expectedDownloads.values())if(expectation.url===p.url&&expectation.watchId!==p.watchId)throw new Error('Download URL is ambiguous between active tab owners');
   expectedDownloads.set(request.sessionId,{tabId:debuggee.tabId,watchId:p.watchId,url:p.url,sessionId:request.sessionId,owner:{...context},created:Date.now()});return{};
  }
  if(method==='Skyre.downloadForget'){expectedDownloads.delete(request.sessionId);for(const[id,value]of activeDownloads)if(value.sessionId===request.sessionId)activeDownloads.delete(id);return{};}

  if(method==='Target.attachToTarget'){const frame=frameTargets.get(p.targetId);if(!frame||frame.tabId!==debuggee.tabId)throw new Error('Target does not belong to this debugger session');return{sessionId:frame.sessionId};}
  // Browser-global commands must take the checked top-level path. Target discovery
  // and auto-attachment are allowed only for iframe descendants of this root.
  if(method.startsWith('Browser.')||method.startsWith('Target.')&&!['Target.setAutoAttach','Target.getTargetInfo'].includes(method))throw new Error('Global command is not allowed through a page session');
  if(method==='Target.setAutoAttach')return debuggerCommand(context,request.sessionId,debuggee,method,{...p,flatten:true,filter:[{type:'iframe',exclude:false},{exclude:true}]});
  if(method==='Target.getTargetInfo'&&p.targetId){const frame=frameTargets.get(p.targetId);if(p.targetId!==String(debuggee.tabId)&&frame?.tabId!==debuggee.tabId)throw new Error('Target is not owned by this debugger session');}
  if(!['Accessibility','Animation','Audits','CSS','DOM','DOMDebugger','DOMSnapshot','Emulation','Fetch','Input','Inspector','LayerTree','Log','Network','Overlay','Page','Performance','Profiler','Runtime','Debugger','Security','WebAudio','WebMCP','Target'].includes(method.split('.')[0])||['Network.getAllCookies','Network.getCookies','Network.setCookie','Network.setCookies','Network.deleteCookies','Network.clearBrowserCookies','Network.clearBrowserCache'].includes(method))throw new Error('Command has browser-global authority outside the tab lease');
  if(method==='Page.navigate'&&p.url!==undefined)supportedUrl(p.url);
  if(!debuggee.sessionId&&method==='Emulation.setDeviceMetricsOverride')await leases.viewport(context,debuggee.tabId,p);
  if(!debuggee.sessionId&&method==='Emulation.clearDeviceMetricsOverride')await leases.viewport(context,debuggee.tabId,null);
  return debuggerCommand(context,request.sessionId,debuggee,method,p);
 }
 switch(method){
  case 'Target.getTargets':return{targetInfos:[...(await leases.list(context)).map(info),...[...frameTargets.values()].filter(f=>{try{leases.require(context,f.tabId);return true}catch{return false}}).map(f=>f.info)]};
  case 'Target.getTargetInfo':{const id=tabId(p.targetId);leases.require(context,id);return{targetInfo:info(await chrome.tabs.get(id))};}
  case 'Target.createTarget':{const url=supportedUrl(p.url??'about:blank'),tab=await chrome.tabs.create({url,active:false});try{await leases.claim(context,tab.id,'agent');}catch(error){await chrome.tabs.remove(tab.id);throw error;}await groupTab(context,tab.id);return{targetId:String(tab.id)};}
  case 'Target.closeTarget':{const id=tabId(p.targetId);leases.require(context,id);await chrome.tabs.remove(id);forgetTab(id);await leases.remove(id);return{success:true};}
  case 'Target.activateTarget':{const id=tabId(p.targetId);leases.require(context,id);const tab=await chrome.tabs.update(id,{active:true});await chrome.windows.update(tab.windowId,{focused:true});await leases.activate(context,id);return{};}
  case 'Target.attachToTarget':{const frame=frameTargets.get(p.targetId);if(frame){leases.require(context,frame.tabId);return{sessionId:frame.sessionId};}return attach(context,tabId(p.targetId));}
  case 'Target.detachFromTarget':{const target=requireSession(context,p.sessionId);if(target.sessionId){await chrome.debugger.sendCommand({tabId:target.tabId},'Target.detachFromTarget',{sessionId:target.sessionId});sessions.delete(p.sessionId);for(const[id,frame]of frameTargets)if(frame.sessionId===target.sessionId)frameTargets.delete(id);}else await detachTab(target.tabId);return{};}
  case 'Browser.getWindowForTarget':{const id=tabId(p.targetId);leases.require(context,id);const tab=await chrome.tabs.get(id),window=await chrome.windows.get(tab.windowId);return{windowId:window.id,bounds:{left:window.left,top:window.top,width:window.width,height:window.height,windowState:window.state}};}
  case 'Browser.setWindowBounds':{const tabs=await leases.list(context);if(!tabs.some(tab=>tab.windowId===p.windowId))throw new Error('Window has no tab owned by this session');const bounds=p.bounds??{},updated={};for(const key of ['left','top','width','height'])if(bounds[key]!==undefined)updated[key]=bounds[key];if(bounds.windowState)updated.state=bounds.windowState;await chrome.windows.update(p.windowId,updated);return{};}
  case 'Browser.getVersion':return{protocolVersion:'1.3',product:'SkyreChromeExtension/'+chrome.runtime.getManifest().version,userAgent:navigator.userAgent,jsVersion:'unknown',revision:'independent'};
  case 'Browser.setDownloadBehavior':{const error=new Error('Chrome extension cannot select arbitrary absolute download directories; use a browser-level CDP endpoint for owned downloads');error.code=-32601;throw error;}
  default:{const error=new Error('Browser-level CDP method unsupported by the extension adapter: '+method);error.code=-32601;throw error;}
 }
}
chrome.debugger.onEvent.addListener((source,method,params)=>{
 const sessionId=source.sessionId??'tab-'+source.tabId,lease=leases.snapshotLease(source.tabId);if(!lease||lease.state!=='active'||leases.sessions.get(lease.sessionId)?.turnId!==lease.turnId||!sessions.has('tab-'+source.tabId))return;
 if(!sessions.has(sessionId)&&source.sessionId)sessions.set(sessionId,{tabId:source.tabId,sessionId:source.sessionId});
 if(method==='Target.attachedToTarget'&&params.sessionId&&params.targetInfo?.type==='iframe'){sessions.set(params.sessionId,{tabId:source.tabId,sessionId:params.sessionId});frameTargets.set(params.targetInfo.targetId,{sessionId:params.sessionId,tabId:source.tabId,info:params.targetInfo});}
 if(method==='Target.detachedFromTarget'){sessions.delete(params.sessionId);for(const[id,frame]of frameTargets)if(frame.sessionId===params.sessionId)frameTargets.delete(id);}
 send({sessionId,method,params});
});
chrome.debugger.onDetach.addListener((source,reason)=>{for(const[id,target]of [...sessions])if(target.tabId===source.tabId)send({method:'Target.detachedFromTarget',params:{sessionId:id,targetId:String(source.tabId),reason}});forgetTab(source.tabId);});
chrome.tabs.onRemoved.addListener(id=>{const wasOwned=leases.snapshotLease(id);forgetTab(id);leases.serial(()=>leases.remove(id)).catch(()=>{});if(wasOwned)send({method:'Target.targetDestroyed',params:{targetId:String(id)}});});
chrome.tabs.onReplaced.addListener((added,removed)=>{forgetTab(removed);leases.serial(()=>leases.replace(added,removed)).catch(()=>{});});
chrome.tabs.onCreated.addListener(tab=>{const snapshot=leases.snapshotLease(tab.openerTabId);if(!snapshot)return;leases.serial(async()=>{if(await leases.adopt(snapshot,tab.id))await groupTab(snapshot,tab.id);}).catch(()=>{});});
function downloadOwnerValid(value){try{leases.require(value.owner,value.tabId);return sessions.has(value.sessionId);}catch{return false;}}
chrome.downloads?.onCreated.addListener(item=>leases.serial(async()=>{
 if(!Number.isSafeInteger(item.id)||item.id<0||activeDownloads.size>=1024)return;
 const url=item.finalUrl??item.url,candidates=[...expectedDownloads.values()].filter(value=>value.url===url&&downloadOwnerValid(value)&&Date.now()-value.created<=120000);
 if(candidates.length!==1)return;
 const value=candidates[0];expectedDownloads.delete(value.sessionId);activeDownloads.set(item.id,{...value,filename:typeof item.filename==='string'?item.filename:undefined});
 send({sessionId:value.sessionId,method:'Skyre.downloadChange',params:{id:String(item.id),watchId:value.watchId,url,filename:typeof item.filename==='string'?item.filename:undefined,status:'started'}});
}).catch(()=>{}));
chrome.downloads?.onChanged.addListener(delta=>leases.serial(async()=>{
 const value=activeDownloads.get(delta.id);if(!value)return;if(!downloadOwnerValid(value)){activeDownloads.delete(delta.id);return;}
 if(typeof delta.filename?.current==='string')value.filename=delta.filename.current;
 const state=delta.state?.current;if(!['complete','interrupted','in_progress'].includes(state))return;const status=state==='complete'?'complete':state==='interrupted'?(delta.error?.current==='USER_CANCELED'?'canceled':'failed'):'in_progress';
 send({sessionId:value.sessionId,method:'Skyre.downloadChange',params:{id:String(delta.id),watchId:value.watchId,url:value.url,filename:value.filename,status}});
 if(status!=='in_progress')activeDownloads.delete(delta.id);
}).catch(()=>{}));

connect();
