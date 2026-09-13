import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import {webcrypto} from 'node:crypto';
const source=fs.readFileSync(new URL('../extensions/chrome/background.js',import.meta.url),'utf8');
const leaseSource=fs.readFileSync(new URL('../extensions/chrome/leases.js',import.meta.url),'utf8');
const event=()=>({listeners:[],addListener(fn){this.listeners.push(fn);},emit(...args){for(const fn of this.listeners)fn(...args);}});
const clone=value=>structuredClone(value);
const owner=(sessionId='session-a',turnId='turn-1',mode='current')=>({sessionId,turnId,mode});
function fixture(shared={local:{},session:{},tabs:new Map([[7,{id:7,windowId:2,url:'https://owned.test',title:'Owned',groupId:-1,lastAccessed:2000}]]),groups:new Map(),next:8}){
 const sent=[],calls=[],failure={},timers=new Map();let context,timerId=0;
 const invoke=(name,args,fn)=>{calls.push([name,...clone(args)]);if(failure[name]){const error=failure[name];delete failure[name];throw Error(error);}return fn();};
 const storage=kind=>({get:async key=>clone({[key]:shared[kind][key]}),set:async value=>invoke('storage.'+kind,[value],()=>Object.assign(shared[kind],clone(value)))});
 const port={onMessage:event(),onDisconnect:event(),postMessage:x=>sent.push(clone(x)),disconnect(){calls.push(['disconnect']);}};
 const chrome={action:{onClicked:event(),setBadgeText:async args=>invoke('badge',[args],()=>{})},runtime:{connectNative:name=>{assert.equal(name,'org.nanocodex.computer');return port;},onStartup:event(),onInstalled:event(),getManifest:()=>({version:'0.1.0'})},storage:{local:storage('local'),session:storage('session')},tabs:{
  query:async()=>[...shared.tabs.values()].map(clone),get:async id=>{if(!shared.tabs.has(id))throw Error('No tab');return clone(shared.tabs.get(id));},
  create:async args=>invoke('create',[args],()=>{const tab={id:shared.next++,windowId:2,groupId:-1,...args};shared.tabs.set(tab.id,tab);return clone(tab)}),
  update:async(id,args)=>invoke('update',[id,args],()=>{Object.assign(shared.tabs.get(id),args);return clone(shared.tabs.get(id));}),
  remove:async id=>invoke('remove',[id],()=>shared.tabs.delete(id)),
  group:async args=>invoke('group',[args],()=>{const id=args.groupId??(shared.groups.size+1);shared.groups.set(id,shared.groups.get(id)??{id,windowId:2,title:''});for(const tab of args.tabIds)shared.tabs.get(tab).groupId=id;return id;}),
  ungroup:async id=>invoke('ungroup',[id],()=>{shared.tabs.get(id).groupId=-1}),onRemoved:event(),onReplaced:event(),onCreated:event()},
  tabGroups:{get:async id=>{if(!shared.groups.has(id))throw Error('No group');return clone(shared.groups.get(id))},update:async(id,args)=>invoke('group.update',[id,args],()=>{Object.assign(shared.groups.get(id),args);return clone(shared.groups.get(id));})},
  windows:{get:async id=>({id,left:0,top:0,width:800,height:600,state:'normal'}),update:async(id,args)=>invoke('window',[id,args],()=>{})},
  downloads:{onCreated:event(),onChanged:event()},
  history:{search:async args=>[{url:'https://owned.test',title:args.text,lastVisitTime:1000}]},
  debugger:{attach:async(target,version)=>invoke('attach',[target,version],()=>{}),detach:async target=>invoke('detach',[target],()=>{}),sendCommand:async(target,method,args)=>invoke('cdp',[target,method,args],()=>({ok:true})),onEvent:event(),onDetach:event()}};
 context=vm.createContext({chrome,navigator:{userAgent:'Owned Chrome'},TextEncoder,TextDecoder,Uint8Array,btoa,atob,setTimeout:(fn)=>{const id=++timerId;timers.set(id,fn);return id;},clearTimeout:id=>timers.delete(id),console,structuredClone,crypto:webcrypto,URL,importScripts:name=>{assert.equal(name,'leases.js');vm.runInContext(leaseSource,context);}});vm.runInContext(source,context);
 return {shared,context,port,sent,calls,chrome,failure,timers,run:(method,params={},sessionId,ctx=owner())=>vm.runInContext(`dispatch(${JSON.stringify({id:1,method,params,sessionId,_skyreContext:ctx})})`,context),state:()=>clone(vm.runInContext('({leases:[...leases.leases],sessions:[...leases.sessions]})',context)),idle:()=>vm.runInContext('leases.queue',context)};
}
async function claimed(){const f=fixture();await f.run('Skyre.claimTab',{tab:'7'});return f;}
function child(f,id='frame-id',sessionId='child-frame-session'){f.chrome.debugger.onEvent.emit({tabId:7},'Target.attachedToTarget',{sessionId,targetInfo:{targetId:id,type:'iframe',url:'https://child.test'}});}

test('ownership checks cover discovery, claim, attach, global escape and competing sessions',async()=>{
 const f=fixture();assert.equal((await f.run('Target.getTargets')).targetInfos.length,0);
 for(const method of ['Target.attachToTarget','Target.closeTarget','Target.activateTarget','Target.getTargetInfo','Browser.getWindowForTarget'])await assert.rejects(f.run(method,{targetId:'7'}),/not owned/);
 await assert.rejects(f.run('Browser.setWindowBounds',{windowId:2}),/no tab owned/);
 await f.run('Skyre.claimTab',{tab:'7'});assert.equal((await f.run('Target.getTargets')).targetInfos[0].targetId,'7');
 assert.equal((await f.run('Target.attachToTarget',{targetId:'7'})).sessionId,'tab-7');child(f);
 assert.equal((await f.run('Target.attachToTarget',{targetId:'frame-id'},'tab-7')).sessionId,'child-frame-session');await f.run('Runtime.evaluate',{expression:'1'},'child-frame-session');assert.equal(f.calls.at(-1)[1].sessionId,'child-frame-session');
 await assert.rejects(f.run('Target.closeTarget',{targetId:'7'},'tab-7'),/Global command/);
 await assert.rejects(f.run('Target.attachToTarget',{targetId:'other'},'tab-7'),/does not belong/);
 await assert.rejects(f.run('Skyre.claimTab',{tab:'7'},undefined,owner('other')),/already owned/);
 await assert.rejects(f.run('Runtime.evaluate',{},'tab-7',owner('other')),/not owned/);
 await assert.rejects(f.run('Target.getTargets',{},undefined,null),/context/);
 await f.run('Target.closeTarget',{targetId:'7'});assert.ok(!f.shared.tabs.has(7));assert.equal(f.state().leases.length,0);
});
test('detached sessions cannot execute and iframe discovery reuses child identity',async()=>{const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});child(f);assert.ok((await f.run('Target.getTargets')).targetInfos.some(t=>t.targetId==='frame-id'));assert.equal((await f.run('Target.attachToTarget',{targetId:'frame-id'})).sessionId,'child-frame-session');f.chrome.debugger.onEvent.emit({tabId:7},'Page.loadEventFired',{timestamp:1});assert.equal(f.sent.at(-1).sessionId,'tab-7');f.chrome.debugger.onDetach.emit({tabId:7},'target_closed');await assert.rejects(f.run('Runtime.evaluate',{},'tab-7'),/detached/);assert.equal(f.sent.at(-1).method,'Target.detachedFromTarget');assert.ok(!(await f.run('Target.getTargets')).targetInfos.some(t=>t.targetId==='frame-id'));});
test('large messages chunk below Chrome limit and reassemble Unicode',()=>{const f=fixture();vm.runInContext(`send({text:'α🧪'.repeat(250000)})`,f.context);assert.ok(f.sent.length>1);let value;for(const chunk of f.sent)value=vm.runInContext(`receive(${JSON.stringify(chunk)})`,f.context);assert.equal(value.text,'α🧪'.repeat(250000));assert.ok(f.sent.every(x=>JSON.stringify(x).length<1024*1024));});
test('inconsistent chunks and unsupported download authority fail explicitly',async()=>{const f=fixture();assert.throws(()=>vm.runInContext('receive({__skyre_chunk:{id:1,part:1,total:2,data:""}})',f.context),/Missing chunk/);await assert.rejects(f.run('Browser.setDownloadBehavior',{downloadPath:'/arbitrary'}),error=>error.code===-32601);await f.run('Skyre.claimTab',{tab:'7'});await f.run('Target.attachToTarget',{targetId:'7'});await assert.rejects(f.run('Page.setDownloadBehavior',{behavior:'allow',downloadPath:'/arbitrary'},'tab-7'),error=>error.code===-32601);});
test('worker restart and native disconnect retain persisted ownership; installation change fails closed',async()=>{const f=await claimed(),saved=f.state().leases[0][1];await f.run('Target.attachToTarget',{targetId:'7'});f.port.onDisconnect.emit();await f.idle();assert.ok(f.calls.some(c=>c[0]==='detach'));assert.equal(f.state().leases[0][1].claimedAt,saved.claimedAt);const restarted=fixture(f.shared);assert.equal((await restarted.run('Target.getTargets')).targetInfos[0].targetId,'7');await assert.rejects(restarted.run('Skyre.claimTab',{tab:'7'},undefined,owner('other')),/already owned/);f.shared.local['skyre.provider.instance.v1']='changed-instance';const changed=fixture(f.shared);await assert.rejects(changed.run('Target.getTargets'),/another extension instance/);});
test('stale cached and retired turns fail; new turn clears marks without changing origin or claim time',async()=>{const f=await claimed();await f.run('Skyre.markTab',{tab:'7',status:'handoff'});const before=f.state().leases[0][1];await assert.rejects(f.run('Target.getTargets',{},undefined,owner('session-a','turn-2','cached')),/not active/);await f.run('Skyre.beginTurn',{},undefined,owner('session-a','turn-2'));const after=f.state().leases[0][1];assert.equal(after.claimedAt,before.claimedAt);assert.equal(after.origin,'user');assert.equal(after.mark,undefined);assert.equal(after.turnId,'turn-2');await assert.rejects(f.run('Target.getTargets'),/stale/);});
test('storage failure rolls back maps and prevents unauthorized Chrome mutation',async()=>{const f=fixture();await f.run('Skyre.beginTurn');const before=f.state();f.failure['storage.session']='disk failed';await assert.rejects(f.run('Skyre.claimTab',{tab:'7'}),/disk failed/);assert.deepEqual(f.state(),before);assert.ok(!f.calls.some(c=>c[0]==='group'));f.failure['storage.session']='disk failed';await assert.rejects(f.run('Target.createTarget',{url:'about:blank'}),/disk failed/);assert.deepEqual([...f.shared.tabs.keys()],[7]);});
test('ordered completion closes unmarked agent tabs, releases user/deliverable tabs, resumes handoffs',async()=>{const f=await claimed();const ordinary=Number((await f.run('Target.createTarget')).targetId),deliverable=Number((await f.run('Target.createTarget')).targetId),handoff=Number((await f.run('Target.createTarget')).targetId);await f.run('Skyre.markTab',{tab:String(deliverable),status:'deliverable'});await f.run('Skyre.markTab',{tab:String(handoff),status:'handoff'});for(const id of [7,ordinary,deliverable,handoff])await f.run('Target.attachToTarget',{targetId:String(id)});f.calls.length=0;await f.run('Skyre.turnEnded');assert.ok(f.shared.tabs.has(7));assert.ok(!f.shared.tabs.has(ordinary));assert.equal(f.shared.tabs.get(deliverable).groupId,-1);assert.deepEqual(f.state().leases.map(([id])=>id),[handoff]);assert.equal(f.state().leases[0][1].state,'handoff');const names=f.calls.map(c=>c[0]);assert.ok(names.lastIndexOf('badge')<names.indexOf('detach'));assert.ok(names.lastIndexOf('detach')<names.indexOf('ungroup'));assert.ok(names.indexOf('ungroup')<names.indexOf('remove'));await assert.rejects(f.run('Target.getTargets'),/stale/);await f.run('Skyre.beginTurn',{},undefined,owner('session-a','turn-2'));const lease=f.state().leases[0][1];assert.equal(lease.state,'active');assert.equal(lease.mark,undefined);assert.equal(lease.origin,'agent');});
test('cleanup failure preserves active turn and retry reconciles already closed tabs',async()=>{const f=await claimed();const one=Number((await f.run('Target.createTarget')).targetId),two=Number((await f.run('Target.createTarget')).targetId);const remove=f.chrome.tabs.remove;f.chrome.tabs.remove=async id=>{if(id===two){f.chrome.tabs.remove=remove;throw Error('close failed');}return remove(id);};await assert.rejects(f.run('Skyre.turnEnded'),/close failed/);assert.ok(!f.shared.tabs.has(one));assert.equal(f.state().sessions[0][1].turnId,'turn-1');await f.run('Skyre.turnEnded');assert.deepEqual([...f.shared.tabs.keys()],[7]);assert.equal(f.state().leases.length,0);assert.equal(f.calls.filter(c=>c[0]==='remove'&&c[1]===one).length,1);});
test('handoff can be claimed by another owner while active tabs cannot',async()=>{const f=await claimed();await f.run('Skyre.markTab',{tab:'7',status:'handoff'});await f.run('Skyre.turnEnded');await f.run('Skyre.claimTab',{tab:'7'},undefined,owner('other'));assert.equal(f.state().leases[0][1].sessionId,'other');await assert.rejects(f.run('Skyre.claimTab',{tab:'7'},undefined,owner('session-a','turn-2')),/already owned/);});
test('child adoption and replacement preserve authority and explicit viewport while stale events do not',async()=>{const f=await claimed();await f.run('Skyre.setViewport',{tab:'7',value:{width:800,height:600}});f.shared.tabs.set(8,{id:8,openerTabId:7,windowId:2,groupId:-1});f.chrome.tabs.onCreated.emit(f.shared.tabs.get(8));await f.idle();assert.deepEqual(f.state().leases.find(([id])=>id===8)[1].viewportSize,{width:800,height:600});assert.equal(f.state().leases.find(([id])=>id===8)[1].origin,'agent');const snapshot=vm.runInContext('leases.snapshotLease(7)',f.context);await f.run('Skyre.beginTurn',{},undefined,owner('session-a','turn-2'));f.context.oldSnapshot=snapshot;assert.equal(await vm.runInContext('leases.serial(()=>leases.adopt(oldSnapshot,10))',f.context),false);f.shared.tabs.set(9,{id:9,windowId:2});f.shared.tabs.delete(8);f.chrome.tabs.onReplaced.emit(9,8);await f.idle();assert.equal(f.state().leases.find(([id])=>id===9)[1].tabId,9);assert.ok(!f.state().leases.some(([id])=>id===8));f.shared.tabs.delete(9);f.chrome.tabs.onRemoved.emit(9);await f.idle();assert.ok(!f.state().leases.some(([id])=>id===9));});
test('pending viewport is consumed once on attach; reset overrides inherited value',async()=>{const f=fixture();await f.run('Skyre.setViewport',{value:{width:640,height:480}});await f.run('Skyre.claimTab',{tab:'7'});await f.run('Target.attachToTarget',{targetId:'7'});assert.ok(f.calls.some(c=>c[0]==='cdp'&&c[2]==='Emulation.setDeviceMetricsOverride'&&c[3].width===640));assert.equal(f.state().sessions[0][1].pendingViewport,undefined);await f.run('Skyre.setViewport',{tab:'7',value:null});assert.equal(f.calls.at(-1)[2],'Emulation.clearDeviceMetricsOverride');});
test('internal pages reject before claim; internal new-tab transforms and user/history records match schema',async()=>{const f=fixture();f.shared.tabs.get(7).url='chrome://settings';await assert.rejects(f.run('Skyre.claimTab',{tab:'7'}),/Internal/);assert.equal(f.state().leases.length,0);f.shared.tabs.get(7).url='chrome://newtab/';await f.run('Skyre.claimTab',{tab:'7'});assert.equal(f.shared.tabs.get(7).url,'about:blank');await f.run('Skyre.nameSession',{name:'Owned Session'});const tab=(await f.run('Skyre.openTabs')).tabs[0];assert.equal(tab.id,'7');assert.equal(tab.providerTabId,'7');assert.equal(tab.tabGroup,undefined);const agent=await f.run('Target.createTarget');assert.equal((await f.run('Skyre.openTabs')).tabs.find(t=>t.id===agent.targetId).tabGroup,'Owned Session');assert.equal(tab.lastOpened,new Date(2000).toISOString());const history=(await f.run('Skyre.history',{queries:['a','b'],limit:1})).items;assert.equal(history.length,1);assert.equal(history[0].dateVisited,new Date(1000).toISOString());});

test('finalize replaces earlier marks and validation causes no partial mutation',async()=>{const f=fixture();const one=(await f.run('Target.createTarget')).targetId,two=(await f.run('Target.createTarget')).targetId;await f.run('Skyre.markTab',{tab:one,status:'handoff'});const before=f.state();await assert.rejects(f.run('Skyre.finalizeTabs',{keep:[{tabId:two,status:'deliverable'},{tabId:'99',status:'handoff'}]}),/not owned/);assert.deepEqual(f.state(),before);await f.run('Skyre.finalizeTabs',{keep:[{tabId:two,status:'deliverable'}]});assert.ok(!f.shared.tabs.has(Number(one)));assert.ok(f.shared.tabs.has(Number(two)));assert.equal(f.state().leases.length,0);});
test('group cleanup failure stops before close; badges and debugger detach are best effort',async()=>{const f=fixture();const deliverable=(await f.run('Target.createTarget')).targetId,ordinary=(await f.run('Target.createTarget')).targetId;await f.run('Skyre.markTab',{tab:deliverable,status:'deliverable'});await f.run('Target.attachToTarget',{targetId:ordinary});f.failure.badge='badge failed';f.failure.detach='detach failed';f.failure.ungroup='ungroup failed';await assert.rejects(f.run('Skyre.turnEnded'),/ungroup failed/);assert.ok(f.shared.tabs.has(Number(ordinary)));assert.equal(f.state().sessions[0][1].turnId,'turn-1');await f.run('Skyre.turnEnded');assert.ok(!f.shared.tabs.has(Number(ordinary)));});
test('lease-bound page session rejects global cookie/storage commands',async()=>{const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});for(const method of ['Storage.clearDataForOrigin','Network.clearBrowserCookies','Network.setCookie','Network.getAllCookies','SystemInfo.getInfo'])await assert.rejects(f.run(method,{},'tab-7'),/browser-global authority/);});
test('marking does not select a tab and viewport targets only the logical active tab',async()=>{const f=fixture();const one=(await f.run('Target.createTarget')).targetId,two=(await f.run('Target.createTarget')).targetId;await f.run('Skyre.markTab',{tab:one,status:'handoff'});assert.equal(f.state().sessions[0][1].activeTabId,Number(two));await f.run('Skyre.setViewport',{value:{width:320,height:240}});assert.equal(f.state().leases.find(([id])=>id===Number(one))[1].viewportSize,undefined);assert.deepEqual(f.state().leases.find(([id])=>id===Number(two))[1].viewportSize,{width:320,height:240});});
test('handoff transfer revokes stale managed-group handles before rename or reuse',async()=>{const f=fixture();const id=(await f.run('Target.createTarget')).targetId,oldGroup=f.shared.tabs.get(Number(id)).groupId;await f.run('Skyre.markTab',{tab:id,status:'handoff'});await f.run('Skyre.turnEnded');await f.run('Skyre.claimTab',{tab:id},undefined,owner('other'));await f.run('Skyre.beginTurn',{},undefined,owner('session-a','turn-2'));await f.run('Skyre.nameSession',{name:'Different owner'},undefined,owner('session-a','turn-2'));assert.notEqual(f.shared.groups.get(oldGroup).title,'Different owner');const created=(await f.run('Target.createTarget',{},undefined,owner('session-a','turn-2'))).targetId;assert.notEqual(f.shared.tabs.get(Number(created)).groupId,oldGroup);assert.equal(f.shared.tabs.get(Number(id)).groupId,oldGroup);});
test('worker restart recovers only a debugger command channel owned by this extension',async()=>{const f=await claimed();f.failure.attach='Another debugger attached';await f.run('Target.attachToTarget',{targetId:'7'});assert.ok(f.calls.some(c=>c[0]==='cdp'&&c[2]==='Runtime.enable'));assert.ok(f.calls.some(c=>c[0]==='cdp'&&c[2]==='Target.setAutoAttach'&&c[3].autoAttach===false));const other=await claimed();other.failure.attach='Other extension debugger attached';other.failure.cdp='Debugger is not attached';await assert.rejects(other.run('Target.attachToTarget',{targetId:'7'}),/Other extension/);await assert.rejects(other.run('Runtime.evaluate',{},'tab-7'),/detached/);});

const hostLifecycle=(f,phase,turn)=>vm.runInContext(`dispatch(${JSON.stringify({id:1,method:'Skyre.hostLifecycle',params:{phase},_skyreHostAuthorized:true,_skyreContext:owner('session-a',turn)})})`,f.context);
test('trusted host lifecycle is guarded, ordered and idempotent across worker restart',async()=>{
 const f=fixture();await assert.rejects(f.run('Skyre.hostLifecycle',{phase:'started'}),/Trusted extension host/);
 await hostLifecycle(f,'started','host-1');const ordinary=(await f.run('Target.createTarget',{},undefined,owner('session-a','host-1'))).targetId,handoff=(await f.run('Target.createTarget',{},undefined,owner('session-a','host-1'))).targetId;
 await f.run('Skyre.markTab',{tab:handoff,status:'handoff'},undefined,owner('session-a','host-1'));
 await hostLifecycle(f,'ended','host-1');assert.ok(!f.shared.tabs.has(Number(ordinary)));assert.ok(f.shared.tabs.has(Number(handoff)));const count=f.calls.filter(c=>c[0]==='remove').length;
 await hostLifecycle(f,'ended','host-1');assert.equal(f.calls.filter(c=>c[0]==='remove').length,count);
 const restarted=fixture(f.shared);await hostLifecycle(restarted,'ended','host-1');assert.ok(restarted.shared.tabs.has(Number(handoff)));await hostLifecycle(restarted,'started','host-2');assert.equal((await restarted.run('Target.getTargets',{},undefined,owner('session-a','host-2'))).targetInfos[0].targetId,handoff);await assert.rejects(hostLifecycle(restarted,'started','host-1'),/stale/);assert.ok(restarted.shared.tabs.has(Number(handoff)));assert.equal(restarted.state().sessions[0][1].turnId,'host-2');
});

test('native download callbacks require a unique current lease and expire on detach or turn transfer',async()=>{
 const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});
 f.chrome.downloads.onCreated.emit({id:1,finalUrl:'https://owned.test/file',filename:'/fixture/not-owned'});await f.idle();assert.equal(f.sent.filter(e=>e.method==='Skyre.downloadChange').length,0);
 await f.run('Skyre.downloadExpect',{watchId:'download-watch-one',url:'https://owned.test/file'},'tab-7');
 f.chrome.downloads.onCreated.emit({id:2,finalUrl:'https://owned.test/file',filename:''});await f.idle();let event=f.sent.at(-1);assert.equal(event.params.id,'2');assert.equal(event.params.status,'started');assert.equal(event.params.watchId,'download-watch-one');assert.equal(event.params.filename,'');
 f.chrome.downloads.onChanged.emit({id:2,filename:{current:'/fixture/owned'},state:{current:'complete'}});await f.idle();event=f.sent.at(-1);assert.equal(event.params.status,'complete');assert.equal(event.params.filename,'/fixture/owned');
 const before=f.sent.length;f.chrome.downloads.onChanged.emit({id:2,state:{current:'interrupted'}});await f.idle();assert.equal(f.sent.length,before);
 await f.run('Skyre.downloadExpect',{watchId:'download-watch-two',url:'https://owned.test/file'},'tab-7');f.chrome.debugger.onDetach.emit({tabId:7},'detached');const count=f.sent.length;f.chrome.downloads.onCreated.emit({id:3,finalUrl:'https://owned.test/file'});await f.idle();assert.equal(f.sent.length,count);
});
test('extension download expectations reject wrong sessions, child targets and ambiguous URLs without broad download authority',async()=>{
 const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});child(f);
 await assert.rejects(f.run('Skyre.downloadExpect',{watchId:'download-watch-one',url:'https://owned.test/file'},'child-frame-session'),/Child frame/);
 await assert.rejects(f.run('Skyre.downloadExpect',{watchId:'download-watch-one',url:'https://owned.test/file'},'tab-7',owner('other')),/not owned/);
 await f.run('Skyre.downloadExpect',{watchId:'download-watch-one',url:'https://owned.test/file'},'tab-7');const other=(await f.run('Target.createTarget')).targetId;await f.run('Target.attachToTarget',{targetId:other});
 await assert.rejects(f.run('Skyre.downloadExpect',{watchId:'download-watch-two',url:'https://owned.test/file'},'tab-'+other),/ambiguous/);
 await f.run('Skyre.downloadForget',{},'tab-7');await f.run('Skyre.downloadExpect',{watchId:'download-watch-two',url:'https://owned.test/file'},'tab-'+other);
 assert(!f.calls.some(c=>c[0]==='cdp'&&['Browser.setDownloadBehavior','Page.setDownloadBehavior'].includes(c[2])));
});

test('paused navigation releases the ownership queue for authorization and Fetch continuation',async()=>{
 const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});
 let resume,entered;const started=new Promise(resolve=>entered=resolve),send=f.chrome.debugger.sendCommand;
 f.chrome.debugger.sendCommand=(target,method,params)=>{if(method==='Page.navigate'){entered();return new Promise(resolve=>resume=resolve);}if(method==='Fetch.continueRequest')resume({frameId:'root',errorText:'net::ERR_ABORTED'});return send(target,method,params);};
 const navigation=f.run('Page.navigate',{url:'https://owned.test/file'},'tab-7');await started;
 await f.run('Skyre.downloadExpect',{watchId:'download-watch-paused',url:'https://owned.test/file'},'tab-7');
 await f.run('Target.getTargetInfo',{targetId:'7'});
 await f.run('Fetch.continueRequest',{requestId:'paused'},'tab-7');
 assert.equal((await navigation).errorText,'net::ERR_ABORTED');
});

test('pending debugger acknowledgements cannot return results after turn or session revocation',async()=>{
 for(const revoke of ['turn','detach']){
  const f=await claimed();await f.run('Target.attachToTarget',{targetId:'7'});let resolve,entered;const started=new Promise(done=>entered=done),send=f.chrome.debugger.sendCommand;
  f.chrome.debugger.sendCommand=(target,method,params)=>{if(method==='Runtime.evaluate'){entered();return new Promise(done=>resolve=done);}return send(target,method,params);};
  const pending=f.run('Runtime.evaluate',{expression:'owned result'},'tab-7'),rejected=assert.rejects(pending,/not active|ownership changed/);await started;
  if(revoke==='turn')await f.run('Skyre.turnEnded');else{await f.run('Target.detachFromTarget',{sessionId:'tab-7'});await f.run('Target.attachToTarget',{targetId:'7'});}
  resolve({result:{value:'must not cross owner'}});await rejected;
 }
});


test('native asset responses correlate independently and disconnected requests reject',async()=>{
 const f=fixture();const one=vm.runInContext("tabContextAsset('create',{fileName:'one.txt'})",f.context),two=vm.runInContext("tabContextAsset('finish',{assetId:'two'})",f.context);
 const [requestOne,requestTwo]=f.sent.slice(-2);assert.equal(requestOne.method,'codexRuntime/tabContextAsset/create');assert.notEqual(requestOne.id,requestTwo.id);
 f.port.onMessage.emit({jsonrpc:'2.0',id:requestTwo.id,result:{assetId:'two'}});assert.equal((await two).assetId,'two');
 f.port.onMessage.emit({jsonrpc:'2.0',id:requestOne.id,error:{code:1,message:'create failed'}});await assert.rejects(one,error=>error.code===1&&error.message==='create failed');
 const pending=vm.runInContext("tabContextAsset('remove',{assetId:'pending'})",f.context);f.port.onDisconnect.emit();await assert.rejects(pending,/disconnected/);
 assert.equal(vm.runInContext('nativeAssetRequests.size',f.context),0);await assert.rejects(vm.runInContext("tabContextAsset('create',{fileName:'offline'})",f.context),/disconnected/);
 const before=f.sent.length;f.port.onMessage.emit({id:requestOne.id,result:{assetId:'stale'}});assert.equal(f.sent.length,before);
});


test('native asset request timeouts, queue bounds and stale replies release correlation state',async()=>{
 const f=fixture();const pending=Array.from({length:16},()=>vm.runInContext("tabContextAsset('create',{fileName:'queued'})",f.context));
 const rejected=pending.map(p=>assert.rejects(p,/timed out/));
 await assert.rejects(vm.runInContext("tabContextAsset('create',{fileName:'overflow'})",f.context),/Too many pending/);
 const sent=f.sent.length;for(const callback of [...f.timers.values()])callback();await Promise.all(rejected);
 assert.equal(vm.runInContext('nativeAssetRequests.size',f.context),0);
 for(const request of f.sent)f.port.onMessage.emit({id:request.id,result:{assetId:'late'}});assert.equal(f.sent.length,sent);
 const next=vm.runInContext("tabContextAsset('create',{fileName:'next'})",f.context),request=f.sent.at(-1);
 f.port.onMessage.emit({id:request.id,result:{assetId:'next'}});assert.equal((await next).assetId,'next');
});


test('local native-port failures retire pending requests and reconnect without a local onDisconnect event',async()=>{
 const f=await claimed();
 await f.run('Target.attachToTarget',{targetId:'7'});
 const pending=vm.runInContext("tabContextAsset('create',{fileName:'owned.txt'})",f.context);
 const rejected=assert.rejects(pending,/disconnected/);
 f.port.onMessage.emit({__skyre_chunk:{id:1,part:1,total:2,data:'eA=='}});
 await rejected;await f.idle();
 assert.equal(vm.runInContext('port',f.context),undefined);
 assert.equal(vm.runInContext('nativeAssetRequests.size',f.context),0);
 assert(f.calls.some(call=>call[0]==='disconnect'));
 assert(f.calls.some(call=>call[0]==='detach'));
 assert.equal(f.timers.size,1);
 f.port.onDisconnect.emit();
 assert.equal(f.timers.size,1,'late disconnect must not enqueue another reconnect');
 for(const callback of [...f.timers.values()])callback();
 assert.notEqual(vm.runInContext('port',f.context),undefined);
});
