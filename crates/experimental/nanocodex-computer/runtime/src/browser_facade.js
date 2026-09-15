// Independently authored implementation of the retained browser API contract.
// Constructors have no display side effects. The root facade owns discovery/docs.
globalThis.__skyreBrowserFacade = ({rpc, target, bytes, emitImage, emitBrowserDocumentation, trackOperation = promise => promise, deriveOperation = (_,target) => target, ownedYield}) => {
  const locatorDetails = new WeakMap();
  const browserMetadata = new Map();
  const unsupported = {
    'Browser.user':['iab','cdp'], 'Browser.history':['iab','cdp'],
    'Tabs.content':['iab','extension','cdp'], 'Tab.ax':['iab','extension','cdp'],
    'Tab.markDeliverable':['cdp'], 'Tab.markHandoff':['cdp'],
    'Tab.requestManualHandoff':['extension','iab','cdp'],
    'CUAAPI.downloadMedia':['iab'], 'DomCUAAPI.downloadMedia':['iab']
  };
  const invoke = (binding, method, args={}) => {
    const params={...args,...binding};
    if(!browserMetadata.get(binding.browser)?._skyreOriginApproval) return rpc('browser.'+method, params);
    const started=rpc('browser.origin_operation_start',{method,args:params});
    return deriveOperation(started,(async()=>{
      let operation=await started;
      while(operation.pending){
        await ownedYield();
        operation=await rpc('browser.origin_operation_poll',{id:operation.id});
      }
      return operation.value;
    })());
  };
  // The whole readEvents method remains async and is tracked by trackMethods.
  // Do not derive this outer promise from the start or any origin poll: it is
  // an unknown/nonhuman obligation until the entire read settles, even while
  // ownedYield has no provider request in flight.
  const finishRawRead = async (binding, started) => {
    let response=await started, id;
    try {
      while(response && Object.prototype.hasOwnProperty.call(response,'__skyreRawWait')) {
        const packet=response.__skyreRawWait;
        if(Object.keys(response).length!==1 || !packet || Object.keys(packet).length!==1 ||
           typeof packet.id!=='string' || !/^raw-events-[0-9a-f]{64}$/.test(packet.id) ||
           (id!==undefined && packet.id!==id)) throw new Error('Invalid native raw-event continuation');
        id=packet.id;
        if(typeof ownedYield!=='function') throw new Error('Native cooperative yield is unavailable');
        await ownedYield();
        // Captured direct RPC, not invoke(): a continuation cannot become a
        // second origin operation. Native lookup still verifies all authority.
        response=await rpc('browser.cdp_events',{...binding,__skyreRawWait:{id,op:'poll'}});
      }
      id=undefined;
      return response;
    } finally {
      if(id!==undefined) {
        try { await rpc('browser.cdp_events',{...binding,__skyreRawWait:{id,op:'cancel'}}); } catch {}
      }
    }
  };
  const none = async promise => { await promise; };
  const clean = object => Object.fromEntries(Object.entries(object).filter(([,value])=>value!==undefined));
  const trackedMethods = new WeakSet();
  const providerValues = new WeakSet();
  const decoratorTargets = new WeakMap(),decoratorViews = new WeakMap();
  const unwrapArgument = value => {
    if(value===null||typeof value!=='object')return value;
    if(decoratorTargets.has(value))return decoratorTargets.get(value);
    if(providerValues.has(value))return value;
    if(Array.isArray(value))return value.map(unwrapArgument);
    const prototype=Object.getPrototypeOf(value);
    return prototype===null||Object.getPrototypeOf(prototype)===null
      ? Object.fromEntries(Object.entries(value).map(([key,item])=>[key,unwrapArgument(item)])) : value;
  };
  const resolvePromise = Promise.resolve.bind(Promise);
  const trackResult = value => value && typeof value.then==='function' ? deriveOperation(value,resolvePromise(trackOperation(value))) : value;
  const trackMethods = (object, providerApi=false) => {
    for (const key of Object.getOwnPropertyNames(object)) {
      const descriptor=Object.getOwnPropertyDescriptor(object,key),method=descriptor.value;
      if(typeof method!=='function'||trackedMethods.has(method))continue;
      // Captured provider callables are ordinary rest-argument functions named
      // Ko. The Tab decorator adds its distinct descriptor/read view below.
      const wrapped=providerApi ? function Ko(...args){return trackResult(Reflect.apply(method,this,args.map(unwrapArgument)));} : new Proxy(method,{apply(fn,self,args){return trackResult(Reflect.apply(fn,self,args));}});
      trackedMethods.add(wrapped);
      Object.defineProperty(object,key,{...descriptor,value:wrapped});
    }
    return object;
  };
  const api = (object, name, metadata={}) => {
    providerValues.add(object);
    const disabled = new Set(metadata.disabledMemberIds??[]);
    for (const key of Object.keys(object)) {
      const id=name+'.'+key;
      if (disabled.has(id) || (metadata.apiSupportOverrides?.[id] ?? !unsupported[id]?.includes(metadata.type))===false) {
        delete object[key]; continue;
      }
      if (typeof object[key]==='function') Object.defineProperty(object,key,{enumerable:false});
    }
    return trackMethods(object,true);
  };
  const writeState = async (state, options) => { if(options?.emit!==false) await globalThis.nodeRepl?.write?.(state,'cua.state'); };
  const writeImage = async (image, options) => { if(options?.emit!==false) await emitImage(image); };
  const imageBytes = result => bytes({data:result.data,mime_type:'image/png'});
  const required = (value,message) => { if(!value) throw new Error(message); return value; };
  const isRegex = value => { try { Reflect.apply(Object.getOwnPropertyDescriptor(RegExp.prototype,'source').get,value,[]); return true; } catch { return false; } };
  function textToken(value, exact, attribute=false) {
    if(isRegex(value)) {
      if(value.unicode||value.unicodeSets) return String(value);
      return String(value).replace(/(^|[^\\])(\\\\)*(["'`])/g,'$1$2\\\\$3').replace(/>>/g,'\\>\\>');
    }
    const quoted = attribute ? '"'+String(value).replace(/\\/g,'\\\\').replace(/"/g,'\\"')+'"' : JSON.stringify(value);
    return quoted+(exact?'s':'i');
  }
  function finder(kind,value,options={}) {
    if(kind==='Role') {
      required(value,'getByRole requires a role');
      return 'internal:role='+value+(options.name===undefined?'':'[name='+textToken(options.name,!!options.exact,true)+']');
    }
    if(kind==='TestId') return 'internal:testid=[data-testid='+textToken(required(value,'getByTestId requires a testId'),true,true)+']';
    if(typeof value!=='string'&&!isRegex(value)) throw new Error('getBy'+kind+' requires a string or RegExp');
    return kind==='Placeholder' ? 'internal:attr=[placeholder='+textToken(value,!!options.exact,true)+']' : 'internal:'+kind.toLowerCase()+'='+textToken(value,!!options.exact);
  }
  function builders(make) {
    return Object.fromEntries(['Role','Text','Label','Placeholder','TestId'].map(kind=>['getBy'+kind,(value,options={})=>make(finder(kind,value,options))]));
  }
  function evaluateInput(pageFunction,arg,where) {
    // The retained client serializes the argument before checking pageFunction.
    // Forward the serialized value, so getters/toJSON are not invoked again by
    // the private RPC encoder.
    let argument;
    if(arg!==undefined) {
      let serialized;
      try { serialized=JSON.stringify(arg); }
      catch(error) { throw new Error((error instanceof Error?error.message:String(error))+'\nplaywright.evaluate arg must be JSON-serializable'); }
      if(serialized===undefined) throw new Error('playwright.evaluate arg must be JSON-serializable');
      argument=JSON.parse(serialized);
    }
    if(typeof pageFunction==='string'&&!pageFunction) throw new Error(where+' requires a pageFunction');
    if(typeof pageFunction!=='string'&&typeof pageFunction!=='function') throw new Error(where+' requires a string or function');
    const parameters=where==='playwright.evaluate'?'arg':where==='locator.evaluateAll'?'elements,arg':'element,arg';
    return {expression:typeof pageFunction==='function'?pageFunction.toString():'('+parameters+')=>('+pageFunction+')',arg:argument};
  }
  function selections(input) {
    const values=Array.isArray(input)?input:[input];
    if(!values.length) throw new Error('locator.selectOption requires at least one value');
    return values.map(value=>{
      if(typeof value==='string') return {value};
      if(!value||typeof value!=='object') throw new Error('locator.selectOption requires a string or { value?, label?, index? }');
      const result={};
      for(const key of ['value','label']) if(value[key]!==undefined) {
        if(typeof value[key]!=='string') throw new Error('locator.selectOption '+key+' must be a string');
        result[key]=value[key];
      }
      if(value.index!==undefined) {
        if(!Number.isInteger(value.index)||value.index<0) throw new Error('locator.selectOption index must be a non-negative integer');
        result.index=value.index;
      }
      if(!Object.keys(result).length) throw new Error('locator.selectOption requires value, label, or index for each selection');
      return result;
    });
  }
  function locator(binding, selector, metadata, cache) {
    const call=(method,args={})=>invoke(binding,'locator_'+method,{...args,selector});
    const make=(next,nextCache)=>locator(binding,next,metadata,nextCache);
    const descendant=(suffix,cacheAllowed=true)=>make(selector+' >> '+suffix,cacheAllowed&&cache?{...cache,relative:cache.relative?cache.relative+' >> '+suffix:suffix}:undefined);
    const compatible=(other,where)=>{
      const detail=locatorDetails.get(other);
      if(!detail) throw new Error(where+' requires a PlaywrightLocator');
      if(detail.binding.browser!==binding.browser||detail.binding.tab!==binding.tab) throw new Error('Locators must belong to the same tab');
      return detail.selector;
    };
    const read=async(method,args={})=>{
      if(cache) {
        const key=cache.relative??'';
        if(!cache.entries.has(key)) {
          const pending=Promise.resolve(invoke(binding,'locator_read_all',{selector:cache.base,relativeSelector:cache.relative,...args}));
          cache.entries.set(key,pending);
          pending.catch(()=>cache.entries.delete(key));
        }
        const item=(await cache.entries.get(key))[cache.index];
        if(item) return method==='get_attribute'?(Object.prototype.hasOwnProperty.call(item.attributes,args.name)?item.attributes[args.name]??null:null):method==='inner_text'?item.inner_text:item.text_content;
      }
      return call(method,args);
    };
    const act=async(method,args={},label=method,context,capture=false,invalidate=true)=>{
      try { const result=await call(method,args); if(invalidate)cache?.entries.clear(); return capture?result:undefined; }
      catch(error) {
        const prefix=context??('locator.'+label+' failed for selector '+selector);
        let suffix='';
        try {
          const rows=await call('read_all',{timeoutMs:1000});
          const matches=rows.slice(0,5).map(row=>({tag:row.tag,role:row.attributes?.role??null,type:row.attributes?.type??null,ariaLabel:row.attributes?.['aria-label']??null,text:(row.text_content??'').trim().slice(0,120),visible:row.visible,disabled:row.enabled===false}));
          const visibleCount=rows.filter(row=>row.visible).length;
          const message=String(error.message??error);
          const kind=/strict mode violation/i.test(message)?'multiple_matches':/intercept|receives pointer events/i.test(message)?'intercepted':rows.length===0?'no_matches':visibleCount===0?'no_visible_match':'action_failed';
          suffix='\nLocator diagnostics: '+JSON.stringify({kind,action:label,locator:selector,matchCount:rows.length,visibleCount,matches,truncated:rows.length>matches.length});
        } catch {}
        const wrapped=new Error(String(error.message??error)+'\n'+prefix+suffix);
        if(error.code!==undefined) wrapped.code=error.code;
        throw wrapped;
      }
    };
    const result={
      ...builders(suffix=>descendant(suffix)),
      async count(){return call('count');},
      async all(){const count=await call('count'),shared={base:selector,entries:new Map()};return Array.from({length:count},(_,index)=>make(selector+' >> nth='+index,{...shared,index}));},
      async allTextContents({timeoutMs}={}){return call('all_text_contents',{timeoutMs});},
      async textContent({timeoutMs}={}){return read('text_content',{timeoutMs});},
      async innerText({timeoutMs}={}){return read('inner_text',{timeoutMs});},
      async getAttribute(name,{timeoutMs}={}){required(name,'locator.getAttribute requires a name');return read('get_attribute',{name,timeoutMs});},
      async isVisible(){return call('is_visible');}, async isEnabled(){return call('is_enabled');},
      async click(options={}){await act('click',clean({button:options.button,modifiers:options.modifiers,force:options.force,timeoutMs:options.timeoutMs}),'click','waiting on click for selector '+selector);},
      async dblclick(options={}){await act('click',clean({button:options.button,modifiers:options.modifiers,force:options.force,timeoutMs:options.timeoutMs,clickCount:2}),'dblclick','waiting on dblclick for selector '+selector);},
      async fill(value,{timeoutMs}={}){if(value==null)throw new Error('locator.fill requires a value');await act('fill',{value,timeoutMs});},
      async type(value,{timeoutMs}={}){if(value==null)throw new Error('locator.type requires a value');await act('type',{value,timeoutMs},'type');},
      async press(value,{timeoutMs}={}){if(value==null)throw new Error('locator.press requires a value');await act('press',{key:value,timeoutMs},'press');},
      async pressSequentially(value,{timeoutMs}={}){if(value==null)throw new Error('locator.pressSequentially requires a value');await act('press_sequentially',{text:value,timeoutMs},'pressSequentially');},
      async selectOption(value,{timeoutMs}={}){await act('select_option',{values:selections(value),timeoutMs},'selectOption');},
      async setChecked(checked,options={}){if(typeof checked!=='boolean')throw new Error('locator.setChecked requires a boolean');await act('set_checked',{checked,force:options.force,timeoutMs:options.timeoutMs},'setChecked','locator.setChecked('+checked+') failed for selector '+selector);},
      async check(options={}){await result.setChecked(true,options);}, async uncheck(options={}){await result.setChecked(false,options);},
      async waitFor({state,timeoutMs}){required(state,'locator.waitFor requires a state');await act('wait_for',{state,timeoutMs},'waitFor','locator.waitFor('+state+') timed out for selector '+selector,false,false);},
      async downloadMedia({timeoutMs}={}){const watch=await act('download_media',{timeoutMs},'downloadMedia',undefined,true);if(watch?.watchId)await waitDownload(binding,watch);},
      async evaluate(fn,arg,options={}){return invoke(binding,'readonly_evaluate',{selector,...evaluateInput(fn,arg,'locator.evaluate'),timeoutMs:options?.timeoutMs});},
      async evaluateAll(fn,arg,options={}){return invoke(binding,'readonly_evaluate',{selector,...evaluateInput(fn,arg,'locator.evaluateAll'),timeoutMs:options?.timeoutMs,all:true});},
      locator(value,options={}){required(value,'locator.locator requires a selector');return descendant(value).filter(options);},
      first(){return descendant('nth=0');}, last(){return descendant('nth=-1');},
      nth(index){if(typeof index!=='number')throw new Error('locator.nth requires a numeric index');return descendant('nth='+index);},
      and(other){return descendant('internal:and='+JSON.stringify(compatible(other,'locator.and')),false);},
      or(other){return descendant('internal:or='+JSON.stringify(compatible(other,'locator.or')),false);},
      filter(options={}){
        const clauses=[];
        for(const [name,engine] of [['hasText','has-text'],['hasNotText','has-not-text']]) if(options[name]!==undefined)clauses.push('internal:'+engine+'='+textToken(options[name],false));
        for(const [name,engine] of [['has','has'],['hasNot','has-not']]) if(options[name]!==undefined)clauses.push('internal:'+engine+'='+JSON.stringify(compatible(options[name],'locator.filter '+name)));
        if(options.visible!==undefined){if(typeof options.visible!=='boolean')throw new Error('locator.filter visible must be a boolean');clauses.push('visible='+options.visible);}
        if(!clauses.length)return make(selector,cache);
        return descendant(clauses.join(' >> '),!cache||cache.relative!==undefined);
      }
    };
    locatorDetails.set(result,{binding,selector});
    return api(result,'PlaywrightLocator',metadata);
  }
  function frameLocator(binding,selector,metadata) {
    const make=value=>locator(binding,selector+' >> internal:control=enter-frame >> '+value,metadata);
    return api({...builders(make),locator(value){return make(required(value,'frameLocator.locator requires a selector'));},frameLocator(value){return frameLocator(binding,selector+' >> internal:control=enter-frame >> '+required(value,'frameLocator.frameLocator requires a selector'),metadata);}},'PlaywrightFrameLocator',metadata);
  }
  async function waitDownload(binding,watch){for(;;){const result=await invoke(binding,'download_poll',{watchId:watch.watchId});if(!result.pending)return result.value;await new Promise(resolve=>setTimeout(resolve,25));}}
  function playwright(binding,metadata) {
    const call=(method,args)=>invoke(binding,method,args);
    const make=value=>locator(binding,value,metadata);
    const point=(value,where)=>{if(!Number.isFinite(value?.x)||!Number.isFinite(value?.y))throw new Error(where+' requires numeric x and y coordinates');return {x:value.x,y:value.y,includeNonInteractable:value.includeNonInteractable};};
    const observeNavigation=async input=>{
      const watch=await call('navigation_arm',input);
      try {
        for(;;){const result=await call('navigation_poll',{watchId:watch.id});if(!result.pending)return;await new Promise(resolve=>setTimeout(resolve,25));}
      } finally {
        // Cleanup cannot replace the action/wait outcome or replay input.
        try { Promise.resolve(call('navigation_cancel',{watchId:watch.id})).catch(()=>{}); } catch {}
      }
    };
    const result={
      ...builders(make),
      locator(value){return make(required(value,'playwright.locator requires a selector'));},
      frameLocator(value){return frameLocator(binding,required(value,'playwright.frameLocator requires a selector'),metadata);},
      async domSnapshot(){const value=await call('dom_snapshot');return typeof value==='string'?value:JSON.stringify(value);},
      async elementInfo(options){return call('element_info',point(options,'playwright.elementInfo'));},
      async elementScreenshot(options){return imageBytes(await call('element_screenshot',point(options,'playwright.elementScreenshot')));},
      async evaluate(fn,arg,options={}){return call('readonly_evaluate',{...evaluateInput(fn,arg,'playwright.evaluate'),timeoutMs:options?.timeoutMs});},
      async waitForURL(url,options={}){required(url,'playwright.waitForURL requires a url');await observeNavigation({url,waitUntil:options.waitUntil,timeoutMs:options.timeoutMs});},
      async waitForLoadState(options={}){await observeNavigation({state:options.state,timeoutMs:options.timeoutMs});},
      async waitForTimeout(timeoutMs){if(!Number.isInteger(timeoutMs)||timeoutMs<0)throw new Error('playwright.waitForTimeout requires a non-negative integer');await call('wait_for_timeout',{timeoutMs});},
      async expectNavigation(action,options={}){

        // Enqueue observation first, then invoke the callback synchronously like
        // the original client. Polling leaves the serial provider available to
        // callbacks that await other asynchronous work before navigating.
        const observed=options.url ? result.waitForURL(options.url,{timeoutMs:options.timeoutMs,waitUntil:options.waitUntil}) : result.waitForLoadState({timeoutMs:options.timeoutMs,state:options.waitUntil});
        const actionResult=action();
        return (await Promise.all([actionResult,observed]))[0];
      },
      async waitForEvent(event,options={}){
        if(!['download','filechooser'].includes(event))throw new Error("playwright.waitForEvent only supports 'download' and 'filechooser'");
        const armed=call(event==='download'?'download_arm':'file_chooser_enable',{timeoutMs:options.timeoutMs});
        const watch=await armed;
        let observed;
        for(;;){const result=await call(event==='download'?'download_poll':'file_chooser_poll',{watchId:watch.watchId});if(!result.pending){observed=result.value;break;}await new Promise(resolve=>setTimeout(resolve,25));}
        if(event==='download')return api({async path({timeoutMs}={}){return (await call('download_path',{downloadId:observed.guid??observed.download_id,timeoutMs})).path??null;}},'PlaywrightDownload',metadata);
        const chooserId=observed.id??observed.file_chooser_id;
        return api({isMultiple(){return observed.is_multiple??observed.mode==='selectMultiple';},async setFiles(files,{timeoutMs}={}){
          if(files==null)throw new Error('fileChooser.setFiles requires files');const list=Array.isArray(files)?files:[files];if(!list.length)throw new Error('fileChooser.setFiles requires at least one file');
          try {await call('file_chooser_set_files',{files:list,fileChooserId:chooserId,timeoutMs});}catch(error){throw new Error(error.message+'\nfileChooser.setFiles failed');}
        }},'PlaywrightFileChooser',metadata);
      },
      async goBack(){await call('back');},async goForward(){await call('forward');}
    };
    return api(result,'PlaywrightAPI',metadata);
  }
  function capabilities(binding,scope,infos=[],metadata={}) {
    const supported={};
    const descriptions={visibility:"Use to show or hide the browser to the user, and to determine the browser's current visibility. Keep browser work in the background unless the user asks to see it or live viewing is useful. When the browser should be visible, call set(true).",viewport:"Controls an explicit browser viewport override for responsive or device-size testing. Use it when a task calls for specific dimensions or breakpoint validation; otherwise leave it unset so the browser uses its normal viewport. Reset temporary overrides before finishing unless the user asked to keep them.",cdp:"Send raw Chrome DevTools Protocol commands and read debugger events through a supported tab for developer use cases."};
    for(const info of infos) {
      const id=info.id,call=(method,args)=>invoke(binding,method,args);
      let methods;
      if(scope==='browser'&&id==='visibility') methods={async get(){return (await call('visibility_get')).visible;},async set(visible){await call('visibility_set',{visible});}};
      if(scope==='browser'&&id==='viewport') methods={async set(options){await call('viewport_set',options);},async reset(){await call('viewport_reset');}};
      if(scope==='tab'&&id==='cdp') methods={async send(method,params,options={}){return call('cdp_call',{method,params,target:options.target,timeoutMs:options.timeoutMs});},async readEvents(options={}){
        const after=options?.afterSequence,limit=options?.limit,methods=options?.methods,target=options?.target,timeoutMs=options?.timeoutMs;
        for(const [name,value]of [['afterSequence',after],['timeoutMs',timeoutMs]])if(value!==undefined&&(!Number.isInteger(value)||value<0))throw new Error(`CDP ${name} must be a nonnegative integer`);
        if(limit!==undefined&&(!Number.isInteger(limit)||limit<=0||limit>1000))throw new Error('CDP limit must be an integer between 1 and 1000');
        if(methods!==undefined&&(!Array.isArray(methods)||!methods.length||Array.from(methods).some(value=>typeof value!=='string'||!value.length)))throw new Error('CDP methods must be a nonempty string array');
        let selected;if(target!=null){const sessionId=target.sessionId,targetId=target.targetId;for(const value of [sessionId,targetId])if(value!==undefined&&(typeof value!=='string'||!value.length))throw new Error('CDP target identifiers must be nonempty strings');if((sessionId==null)===(targetId==null))throw new Error('CDP target requires exactly one sessionId or targetId');selected={sessionId,targetId};}
        const hasLoneSurrogate=value=>{for(let i=0;i<value.length;i++){const unit=value.charCodeAt(i);if(unit>=0xd800&&unit<=0xdbff){const next=value.charCodeAt(++i);if(!(next>=0xdc00&&next<=0xdfff))return true;}else if(unit>=0xdc00&&unit<=0xdfff)return true;}return false;};
        const units=value=>Array.from({length:value.length},(_,i)=>value.charCodeAt(i));
        let wireMethods=methods,filter_utf16;
        if(methods?.some(hasLoneSurrogate)){filter_utf16={methods:methods.map(units)};wireMethods=undefined;}
        if(selected){const kind=selected.sessionId!==undefined?'sessionId':'targetId';if(hasLoneSurrogate(selected[kind])){filter_utf16={...filter_utf16,target:{[kind]:units(selected[kind])}};selected=undefined;}}
        return await finishRawRead(binding,call('cdp_events',{after_sequence:after,limit,methods:wireMethods,target:selected,timeoutMs,filter_utf16}));
      }};
      if(scope==='tab'&&id==='webmcp') methods={async fetchTools(){const tools=await call('webmcp_list');const byName=new Map(tools.map(tool=>[tool.name,tool]));return Object.freeze({description:()=>JSON.stringify(tools),async call(name,input,options={}){name=name.trim();const tool=byName.get(name);if(!tool)throw new Error('WebMCP tool '+JSON.stringify(name)+' is not available in this snapshot. Call fetchTools() again.');return call('webmcp_invoke',{name,arguments:input,registrationId:tool.registrationId??tool.registration_id,timeoutMs:options.timeoutMs});}});}};
      if(!methods)continue; // Unknown or unavailable provider capabilities are never invented.
      supported[id]=api({id,info:{...info,description:descriptions[id]??info.description},...methods,async documentation(){return call('get_documentation',{name:'capabilities/'+scope+'/'+id});}},scope==='browser'?'BrowserCapability':'TabCapability',metadata);
    }
    return api({async list(){return Object.values(supported).map(value=>value.info);},async get(id){if(!supported[id])throw new Error('Capability is not available: '+id);return supported[id];}},scope==='browser'?'BrowserCapabilityCollection':'TabCapabilityCollection',metadata);
  }
  function tab(browserId,idOrPayload, suppliedMetadata) {
    const id=typeof idOrPayload==='string'?idOrPayload:idOrPayload?.id;
    if(!id)throw new Error('Tab requires an id');
    const binding={browser:browserId,tab:id},metadata=suppliedMetadata??browserMetadata.get(browserId)??{type:'cdp',apiSupportOverrides:{'Tab.ax':true}};
    const call=(method,args)=>invoke(binding,method,args);
    const axAction=(kind,args)=>none(call('tab_ax_action',{action:{kind,...args}}));
    const ax=api({
      async get(mode='state',options){
        const capture=await call('ax_capture',{content:mode==='state'?'axState':mode==='screenshot'?'screenshot':'axStateAndScreenshot',disableDiffing:options?.disableDiffing});
        if(mode==='state'){if(typeof capture.state!=='string')throw new Error('ax capture returned no accessibility state');return capture.state;}
        if(typeof capture.data!=='string'){
          if(mode==='both'&&typeof capture.screenshot_unavailable==='string'&&typeof capture.state==='string')return {state:capture.state};
          throw new Error('ax capture returned no screenshot data');
        }
        const screenshot=imageBytes(capture);if(mode==='screenshot')return screenshot;
        if(typeof capture.state!=='string')throw new Error('ax capture returned no accessibility state');
        return {state:capture.state,screenshot};
      },
      async write(mode='state',options){const result=await ax.get(mode,options);const display=value=>console.log({type:'value',value:value.length<=100000?value:value.slice(0,100000)+'[truncated '+(value.length-100000)+' chars]'});if(mode==='state')display(result);else if(mode==='screenshot')await emitImage(result);else{display(result.state);if(result.screenshot!==undefined)await emitImage(result.screenshot);}},
      async click(value,options){return axAction('click',{target:value,mouse_button:options?.mouseButton,click_count:options?.clickCount});},
      async drag(from,to){return axAction('drag',{from,to});},
      async pressKey(key){return axAction('press_key',{key});},
      async scroll(value,direction,pages){return axAction('scroll',{target:value,direction,pages});},
      async selectText(elementIndex,text,options){return axAction('select_text',{element_index:elementIndex,text,prefix:options?.prefix,suffix:options?.suffix,selection_type:options?.selectionType});},
      async setValue(elementIndex,value){return axAction('set_value',{element_index:elementIndex,value});},
      async typeText(text){return axAction('type_text',{text});},
      async performSecondaryAction(elementIndex,action){return axAction('perform_secondary_action',{element_index:elementIndex,action});}
    },'AXAPI',metadata);
    const clipboard=api({async read(){return (await call('clipboard_read')).map(item=>({presentationStyle:item.presentationStyle,entries:item.entries.map(entry=>({mimeType:entry.mimeType,text:entry.text,base64:entry.base64}))}));},async readText(){return call('clipboard_read_text');},async write(items){if(!Array.isArray(items)||items.length===0)throw new Error('tab.clipboard.write requires at least one clipboard item');await call('clipboard_write',{items:items.map(item=>({presentationStyle:item.presentationStyle,entries:item.entries.map(entry=>({mimeType:entry.mimeType,text:entry.text,base64:entry.base64}))}))});},async writeText(text){if(text==null)throw new Error('tab.clipboard.writeText requires text');await call('clipboard_write_text',{text});}},'TabClipboardAPI',metadata);
    const cuaCall=(method,options)=>none(call('cua_'+method,options));
    const coordinates=(options,where)=>{if(typeof options?.x!=='number'||typeof options?.y!=='number')throw new Error('cua.'+where+' requires x and y');return {x:options.x,y:options.y};};
    const keys=(options,where)=>{if(!Array.isArray(options?.keys)||!options.keys.length)throw new Error(where+' requires a non-empty keys array');return {keys:options.keys};};
    const cua=api({
      async click(options){return cuaCall('click',{...coordinates(options,'click'),button:options.button,keys:options.keypress});},
      async double_click(options){return cuaCall('double_click',{...coordinates(options,'double_click'),keys:options.keypress});},
      async move(options){return cuaCall('move',{...coordinates(options,'move'),keys:options.keys});},
      async scroll(options){if(typeof options?.x!=='number'||typeof options?.y!=='number'||typeof options?.scrollX!=='number'||typeof options?.scrollY!=='number')throw new Error('cua.scroll requires x, y, scrollX, and scrollY');return cuaCall('scroll',{x:options.x,y:options.y,scroll_x:options.scrollX,scroll_y:options.scrollY,keys:options.keypress});},
      async type(options){if(typeof options?.text!=='string')throw new Error('cua.type requires text');return cuaCall('type',{text:options.text});},
      async keypress(options){return cuaCall('keypress',keys(options,'cua.keypress'));},
      async drag(options){if(!Array.isArray(options?.path)||!options.path.length||options.path.some(point=>typeof point?.x!=='number'||typeof point?.y!=='number'))throw new Error('cua.drag requires a non-empty path of {x, y} points');return cuaCall('drag',{path:options.path,keys:options.keys});},
      async downloadMedia(options){const watch=await call('cua_download_media',{...coordinates(options,'downloadMedia'),timeoutMs:options.timeoutMs});if(watch?.watchId)await waitDownload(binding,watch);}
    },'CUAAPI',metadata);
    const node=(value,where)=>{if(value===undefined)throw new Error(where+' requires a node_id');if(typeof value!=='string')throw new Error(where+' node_id must be a string');if(!value.length)throw new Error(where+' node_id must not be empty');return value;};
    const dom_cua=api({
      async get_visible_dom(){return call('dom_snapshot');},
      async click(options){return none(call('dom_cua_click',{node_id:node(options?.node_id,'dom_cua.click')}));},
      async double_click(options){return none(call('dom_cua_double_click',{node_id:node(options?.node_id,'dom_cua.double_click')}));},
      async scroll({node_id,x,y}){if(typeof x!=='number'||typeof y!=='number')throw new Error('dom_cua.scroll requires x and y numbers');return none(call('dom_cua_scroll',{node_id:node_id===undefined?undefined:node(node_id,'dom_cua.scroll'),scroll_x:x,scroll_y:y}));},
      async type({text}){if(typeof text!=='string')throw new Error('dom_cua.type requires text');return none(call('dom_cua_type',{text}));},
      async keypress(options){return none(call('dom_cua_keypress',keys(options,'dom_cua.keypress')));},
      async downloadMedia(options){const watch=await call('dom_cua_download_media',{node_id:node(options?.node_id,'dom_cua.downloadMedia'),timeoutMs:options.timeoutMs});if(watch?.watchId)await waitDownload(binding,watch);}
    },'DomCUAAPI',metadata);
    const result={id,ax,clipboard,cua,dom_cua,playwright:playwright(binding,metadata),
      capabilities:capabilities(binding,'tab',metadata.capabilities?.tab,metadata),
      content:api({async export(){return (await call('content_export')).path;},async exportGsuite(format){return (await call('export_gsuite',{format})).path;},async exportYouTubeTranscript(){return (await call('export_youtube')).path;}},'ContentAPI',metadata),
      dev:api({async logs(options={}){
        if(options.filter!=null&&typeof options.filter!=='string')throw new Error('tab.dev.logs received an invalid filter');
        if(options.limit!=null&&(!Number.isInteger(options.limit)||options.limit<=0))throw new Error('tab.dev.logs received an invalid limit');
        let levels;if(options.levels!=null){if(!Array.isArray(options.levels)||!options.levels.length)throw new Error('tab.dev.logs received invalid levels');levels=options.levels.map(level=>{if(level==='warning')return 'warn';if(!['debug','info','log','warn','error'].includes(level))throw new Error('tab.dev.logs received invalid level "'+String(level)+'"');return level;});}
        return call('dev_logs',clean({filter:options.filter??undefined,limit:options.limit??undefined,levels}));
      }},'TabDevAPI',metadata),
      goto(url){let operation;const result=(async()=>{required(url,'tab.goto requires a url');await (operation=call('navigate',{url}));})();return operation?deriveOperation(operation,result):result;},
      async back(){await call('back');},async forward(){await call('forward');},async reload(){await call('reload');},async close(){await call('close_tab');},
      async title(){return (await call('get_tab')).title;},async url(){return (await call('get_tab')).url;},
      async markDeliverable(){await call('mark_tab',{status:'deliverable'});},async markHandoff(){await call('mark_tab',{status:'handoff'});},
      async requestManualHandoff(){await call('request_manual_handoff');},
      async screenshot(options={}){if(options.clip&&['x','y','width','height'].some(key=>typeof options.clip[key]!=='number'))throw new Error('tab.screenshot clip requires x, y, width, and height');return imageBytes(await call('screenshot',{fullPage:options.fullPage,clip:options.clip}));},
      async getJsDialog(){const dialog=await call('dialog_get');if(dialog==null)return undefined;const handle=(action,promptText)=>none(call('dialog_handle',{action,promptText,dialogId:dialog.id}));const result={type:dialog.type,dismiss:()=>handle('dismiss')};if(dialog.type==='confirm')result.accept=()=>handle('accept');if(dialog.type==='prompt')result.accept=async text=>{if(typeof text!=='string')throw new Error('prompt.accept requires text');return handle('accept',text);};return api(result,({alert:'AlertDialog',beforeunload:'BeforeUnloadDialog',confirm:'ConfirmDialog',prompt:'PromptDialog'})[dialog.type],metadata);}
    };
    const decorated=api(Object.assign(target?.('browser.',binding)??{},result),'Tab',metadata);
    // The retained core assigns ordinary methods through the client's Tab
    // proxy. Descriptors expose those methods; reads cache a separate wrapper
    // by property name, even if a later assignment replaces the stored method.
    const decoratorNames=new Set(['getAXState','getScreenshot','getAXStateAndScreenshot','paste','click','drag','pressKey','scroll','selectText','setValue','typeText','performSecondaryAction']);
    const decoratorReads=new Map();
    const decoratorResult=value=>{
      if(value===null||typeof value!=='object')return value;
      if(decoratorViews.has(value))return decoratorViews.get(value);
      if(Array.isArray(value))return value.map(decoratorResult);
      if(!(value instanceof Promise))return value;
      // Preserve the client's Promise species and unhandled outer rejection.
      // The private observation drains the inner operation without becoming a
      // second unhandled rejection when the caller catches the public result.
      const result=value.then(decoratorResult);
      resolvePromise(trackOperation(value)).then(()=>{},()=>{});
      return result;
    };
    const publicTab=new Proxy(decorated,{get(object,key,receiver){
      if(!decoratorNames.has(key))return Reflect.get(object,key,receiver);
      const method=Reflect.get(object,key,object);
      if(typeof method!=='function')return method;
      if(decoratorReads.has(key))return decoratorReads.get(key);
      const Ko=(...args)=>decoratorResult(Reflect.apply(method,object,args.map(unwrapArgument)));
      decoratorReads.set(key,Ko);return Ko;
    }});
    providerValues.add(publicTab);
    decoratorTargets.set(publicTab,decorated);decoratorViews.set(decorated,publicTab);
    Object.assign(publicTab,{
      getAXState(options){return(async()=>{const value=await publicTab.ax.get('state',options?.disableDiffing===undefined?undefined:{disableDiffing:options.disableDiffing});await writeState(value,options);return value;})();},
      getScreenshot(options){return(async()=>{const value=await publicTab.ax.get('screenshot');await writeImage(value,options);return value;})();},
      getAXStateAndScreenshot(options){return(async()=>{const value=await publicTab.ax.get('both',options?.disableDiffing===undefined?undefined:{disableDiffing:options.disableDiffing});await writeState(value.state,options);if(value.screenshot!==undefined)await writeImage(value.screenshot,options);return value;})();},
      paste(text,options){return(async()=>{const clipboard=publicTab.clipboard,format=options?.format??'text';if(format==='text'&&clipboard?.writeText!==undefined)await clipboard.writeText(text);else if(clipboard?.write!==undefined)await clipboard.write([{entries:format==='html'?[{mimeType:'text/html',text},{mimeType:'text/plain',text}]:[{mimeType:'text/plain',text}]}]);else if(format==='text')return publicTab.ax.typeText(text);else throw new Error('Browser clipboard does not support '+format+' paste.');await publicTab.ax.pressKey('Ctrl+v');})();},
      click:(value,options)=>publicTab.ax.click(value,options),
      drag:(from,to)=>publicTab.ax.drag(from,to),
      pressKey:key=>publicTab.ax.pressKey(key),
      scroll:(value,direction,pages)=>publicTab.ax.scroll(value,direction,pages),
      selectText:(index,text,options)=>publicTab.ax.selectText(index,text,options),
      setValue:(index,value)=>publicTab.ax.setValue(index,value),
      typeText:text=>publicTab.ax.typeText(text),
      performSecondaryAction:(index,action)=>publicTab.ax.performSecondaryAction(index,action)
    });
    return publicTab;
  }
  function browser(browserId, metadata={type:'cdp',apiSupportOverrides:{'Tab.ax':true}}) {
    browserMetadata.set(browserId,metadata);
    const binding={browser:browserId},call=(method,args)=>invoke(binding,method,args),make=id=>tab(browserId,id,metadata);
    const tabInfo=value=>clean({id:value.id??value.targetId,providerTabId:value.providerTabId,title:value.title,url:value.url});
    const tabs=api({
      async list(){const value=await call('list_tabs');return (value.tabs??value).filter(tab=>!tab.type||tab.type==='page').map(tabInfo);},
      async new(){const value=await call('new_tab');return make(value.id??value.targetId);},
      async get(id){required(id,'tabs.get requires a tab id');const value=await call('get_tab',{tab:id});return make(value.id??value.targetId??id);},
      async selected(){const value=await call('selected_tab');return value&&(value.id??value.targetId??(typeof value==='string'?value:undefined))?make(value.id??value.targetId??value):undefined;},
      async content(options){if(!Array.isArray(options?.urls))throw new Error('tabs.content requires urls');if(!['text','html','domSnapshot'].includes(options.contentType))throw new Error('tabs.content requires a supported contentType');if(!options.urls.length)return [];return call('tabs_content',{urls:options.urls,contentType:options.contentType,timeoutMs:options.timeoutMs});}
    },'Tabs',metadata);
    const user=api({
      async openTabs(){const value=await call('user_open_tabs');return value.tabs??value;},
      async claimTab(value){let id;if(typeof value==='string'){if(!value.length)throw new Error('browser.user.claimTab received an empty tab id');id=value;}else if(value&&typeof value==='object'&&typeof value.id==='string')id=value.id;else throw new Error('browser.user.claimTab expects a tab returned by browser.user.openTabs() or a tab id');const result=await call('user_claim_tab',{tab:id});return make(result.id??result.targetId);}
    },'BrowserUser',metadata);
    return api({browserId,tabs,user,capabilities:capabilities(binding,'browser',metadata.capabilities?.browser,metadata),
      async documentation(){return emitBrowserDocumentation(browserId);},
      async nameSession(name){const trimmed=name.trim();if(!trimmed)throw new Error('browser.nameSession requires a name');await call('name_session',{name:trimmed});},
      async history(options={}){
        if(options===null||Array.isArray(options)||typeof options!=='object')throw new Error('browser.history expects an options object');
        const args={};
        if(options.queries!=null){if(!Array.isArray(options.queries)||!options.queries.length||options.queries.some(value=>typeof value!=='string'))throw new Error('browser.history received invalid queries');args.queries=options.queries;}
        if(options.limit!=null){if(!Number.isInteger(options.limit)||options.limit<=0)throw new Error('browser.history received an invalid limit');args.limit=options.limit;}
        for(const key of ['from','to'])if(options[key]!=null){const date=new Date(options[key]);if(Number.isNaN(date.getTime()))throw new Error('browser.history received an invalid '+key+' date');args[key]=date.toISOString();}
        const result=await call('user_history',args);return result.items??result;
      }
    },'Browser',metadata);
  }
  return {browser,tab};
};
