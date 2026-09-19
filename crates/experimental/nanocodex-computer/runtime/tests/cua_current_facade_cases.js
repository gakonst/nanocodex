// Synthetic providers only. Shared by the installed-provider oracle and QuickJS.
(async () => {
  const events = [], reads = [], native = [], images = [];
  const meta1 = {}, meta2 = {};
  const repl = {requestMeta:meta1,write:async(text,id)=>events.push({text,id}),emitImage:async image=>images.push({bytes:[...image.bytes],mimeType:image.mimeType})};
  globalThis.nodeRepl = repl;
  const readDocumentation = async name => {reads.push(name);return 'doc:' + name;};
  let nativeFailure, browserFailure, userFailure = false, captures = true;
  const computer = {target:'mac',list_apps:async()=>{if(nativeFailure)throw nativeFailure;return computer.target==='linux'?[{id:'fixture.app',name:'Fixture',windows:[]}]:[{id:'fixture.app'}];},
    get_app_state:async args=>{native.push(args);return {app:'fixture.app',text:'fixture state',screenshot:captures?{url:'data:image/png;base64,AQI='}:null,screenshotError:{message:'private backend error',code:42}};},
    drag:async args=>native.push(args),get_desktop_screenshot:async()=>{throw Error('must stay internal');},list_app_windows:async()=>[]};
  const browser = id => ({browserId:id,documentation:async()=> 'browser:' + id,
    tabs:{list:async()=>{if(browserFailure)throw browserFailure;return [{id:'shared',title:'controlled'},{id:'controlled'}];}},
    user:{openTabs:async()=>{if(userFailure)throw Error('user inventory failed');return [{id:'user'},{id:'shared',title:'user'}];},claimTab:async()=>{throw Error('inventory must not claim');}}});
  const browsers = {list:async()=>[{id:'one'},{id:'two'}],get:async id=>browser(id)};
  const api = await globalThis.__testCreateCUA({computer,browsers,readDocumentation,getNodeRepl:()=>repl});
  const keys = Object.keys(api).sort();
  const tabs = await api.listTabs({emit:false});
  const state = await api.getState({emit:false});
  nativeFailure = Error('native unavailable');
  const nativeError = await api.getState({emit:false});
  browserFailure = 'browser unavailable';
  const bothErrors = await api.getState({emit:false});
  nativeFailure = undefined;
  const browserError = await api.getState({emit:false});
  browserFailure = undefined;
  userFailure = true;
  const previousConsole = globalThis.console;
  globalThis.console = {error:()=>{}};
  const userFallback = await api.listTabs({browser:'one',emit:false});
  globalThis.console = previousConsole;
  userFailure = false;
  await api.getBrowser({id:'two'});
  const firstDocs = events.slice();events.length=0;
  await api.rewriteDocumentation();const sameRequest = events.slice();
  repl.requestMeta = meta2;
  await api.rewriteDocumentation();const newRequest = events.slice();events.length=0;
  await api.rewriteDocumentation();const repeatedRequest = events.slice();
  repl.requestMeta = undefined;
  await api.rewriteDocumentation();await api.rewriteDocumentation();const missingRequest = events.slice();events.length=0;
  const app = await api.getApp('Fixture', {get windowId(){throw Error('unsupported options must be ignored');}});
  await app.getAXState({disableDiffing:true,emit:false});
  await app.getScreenshot();
  await app.getAXStateAndScreenshot({emit:false});
  await app.drag([1,2],[3,4],{get mouseButton(){throw Error('unsupported drag options must be ignored');}});
  captures = false;
  let screenshotError;
  try {await app.getScreenshot({emit:false});} catch(error) {screenshotError={message:error.message,code:error.code};}
  const noScreenshot = await app.getAXStateAndScreenshot({emit:false});
  computer.target='linux';
  const linux = await api.getState({emit:false});
  const platformErrors=[];
  for(const method of ['getApp','listApps'])try{await api[method]('Fixture');}catch(error){platformErrors.push(error.message);}
  computer.target='windows';
  const windows = await api.getState({emit:false});
  computer.target='unknown';
  const unknown = await api.getState({emit:false});
  return {keys,tabs,state,nativeError,bothErrors,browserError,userFallback,firstDocs,sameRequest,newRequest,repeatedRequest,missingRequest,reads,native,images,screenshotError,noScreenshot,linux,windows,unknown,platformErrors};
})()
