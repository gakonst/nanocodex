// Independently authored compatibility facade. Rust owns all execution and IO.
(() => {
  const browserFacade = globalThis.__skyreBrowserFacade;
  const privateBridge = globalThis.__skyreTakePrivateBridge?.();
  delete globalThis.__skyreTakePrivateBridge;
  const hostRpc = privateBridge?.rpc ?? globalThis.__skyre_rpc;
  delete globalThis.__skyre_rpc;
  const stringify = privateBridge?.stringify ?? JSON.stringify;
  const parse = privateBridge?.parse ?? JSON.parse;
  const parsePrivate = privateBridge?.parsePrivate ?? parse;
  const getPrivateNodeRepl = () => privateBridge?.nodeRepl ?? globalThis.nodeRepl;
  const trackOperation = privateBridge?.trackOperation ?? (operation=>operation);
  const deriveOperation = privateBridge?.deriveOperation ?? ((_,target)=>target);
  const ownedYield = privateBridge?.ownedYield;
  const setupSurfaces = privateBridge?.setupSurfaces ?? {browser:true,computer:true,error:null};
  let guardianLease;
  const rpc = (method, args = {}) => {
    let wire;
    const result = (async()=>{
    const payload = guardianLease && !method.startsWith('guardian.') ? {...args, guardianLease} : args;
    const response = (method === 'sky.app_policy' ? parsePrivate : parse)(await (wire=hostRpc(method, stringify(payload))));
    if (response.error) {
      const error = new Error(response.error.message);
      // Native formatter failures preserve the previous message-only Error.
      if (response.error.code !== null) error.code = response.error.code;
      throw error;
    }
    return response.result;
    })();
    return wire ? deriveOperation(wire,result) : result;
  };
  function bytes(image) {
    if (image instanceof Uint8Array) return image;
    const table = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
    const data = image.data.replace(/=+$/, '');
    const output = new Uint8Array(Math.floor(data.length * 6 / 8));
    let accumulator = 0, bits = 0, index = 0;
    for (const c of data) {
      const n = table.indexOf(c);
      if (n < 0) throw new Error('Invalid image encoding');
      accumulator = (accumulator << 6) | n;
      bits += 6;
      if (bits >= 8) { bits -= 8; output[index++] = (accumulator >> bits) & 255; }
    }
    return output;
  }
  async function imageFromUrl(url) {
    if (url.startsWith('data:')) {
      // Sky uses Node's forgiving base64 decoder here, including URL alphabet,
      // ignored non-alphabet characters and termination at the first padding.
      // Node truncates UTF-16 code units to bytes before alphabet/padding checks.
      const data = (url.split(',')[1] ?? '').replace(/[\s\S]/g, char => String.fromCharCode(char.charCodeAt(0) & 255)).split('=')[0]
        .replace(/[^A-Za-z0-9+/_-]/g,'').replace(/-/g,'+').replace(/_/g,'/');
      return bytes({data});
    }
    const response = JSON.parse(__skyre_read_image_file(url));
    if (response.error) {const error=new Error(response.error.message);error.code=response.error.code;throw error;}
    return bytes(response.result);
  }
  async function emitState(value, options) {
    if (options?.emit !== false) await globalThis.nodeRepl?.write?.(value, 'cua.state');
  }
  async function emitImage(value, options, mimeType = 'image/png') {
    if (options?.emit !== false) await globalThis.nodeRepl?.emitImage?.({bytes:value,mimeType});
  }
  function nativeTarget(computer, app) {
    const stateArgs = options => options?.disableDiffing === undefined ? {app} : {app,disableDiff:options.disableDiffing};
    return {
      async getAXState(options) {
        const result = await computer.get_app_state(stateArgs(options));
        await emitState(result.text, options);
        return result.text;
      },
      async getScreenshot(options) {
        const result = await computer.get_app_state({app});
        if (result.screenshot === null) {
          throw new Error('Screenshot unavailable for ' + app + '.');
        }
        const screenshot = await imageFromUrl(result.screenshot.url);
        await emitImage(screenshot, options);
        return screenshot;
      },
      async getAXStateAndScreenshot(options) {
        const result = await computer.get_app_state(stateArgs(options));
        await emitState(result.text, options);
        if (result.screenshot === null) return {state:result.text};
        const screenshot = await imageFromUrl(result.screenshot.url);
        await emitImage(screenshot, options);
        return {state:result.text,screenshot};
      },
      paste(text, options) { return computer.paste({app,text,format:options?.format ?? 'text'}); },
      click(value, options) {
        return computer.click({app,...(Array.isArray(value) ? {x:value[0],y:value[1]} : {element_index:value}),
          ...(options?.mouseButton === undefined ? {} : {mouse_button:options.mouseButton}),
          ...(options?.clickCount === undefined ? {} : {click_count:options.clickCount})});
      },
      drag(from,to) { return computer.drag({app,from_x:from[0],from_y:from[1],to_x:to[0],to_y:to[1]}); },
      pressKey(key) { return computer.press_key({app,key}); },
      scroll(value,direction,pages) {
        if (typeof pages === 'object') throw new Error('macOS scroll accepts pages, not pixels.');
        return computer.scroll({app,...(Array.isArray(value) ? {x:value[0],y:value[1]} : {element_index:value}),direction,...(pages === undefined ? {} : {pages})});
      },
      selectText(elementIndex,text,options) {
        return computer.select_text({app,element_index:elementIndex,text,
          ...(options?.prefix === undefined ? {} : {prefix:options.prefix}),
          ...(options?.suffix === undefined ? {} : {suffix:options.suffix}),
          ...(options?.selectionType === undefined ? {} : {selection_type:options.selectionType})});
      },
      setValue(elementIndex,value) { return computer.set_value({app,element_index:elementIndex,value}); },
      typeText(text) { return computer.type_text({app,text}); },
      performSecondaryAction(elementIndex,action) { return computer.perform_secondary_action({app,element_index:elementIndex,action}); }
    };
  }
  function windowId(reference, platform) {
    if (typeof reference !== 'object' || reference === null || !Number.isSafeInteger(reference.windowId) || reference.windowId <= 0)
      throw new Error(platform + ' getApp requires { windowId } from listApps() or listWindows().');
    return reference.windowId;
  }
  function singleScreenshot(screenshots,id) {
    if (screenshots.length > 1) throw new Error('Window ' + id + ' has multiple screenshot regions; a single screenshot is unavailable.');
    return screenshots[0];
  }
  function pixels(value) {
    if (!Number.isFinite(value) || value <= 0) throw new Error('pixels must be a positive finite number.');
    return value;
  }
  function textPaste(options,platform) {
    if (options?.format !== undefined && options.format !== 'text') throw new Error(platform + ' paste supports only text format.');
  }
  function windowsState(state) {
    const ax = state.accessibility;
    if (ax === null) return 'Accessibility state unavailable for window ' + state.window.id + '.';
    const parts = [ax.tree];
    if (ax.focused_element !== undefined) parts.push('Focused element: ' + ax.focused_element);
    if (ax.selected_text !== undefined) parts.push('Selected text: ' + ax.selected_text);
    if (ax.selected_elements !== undefined) parts.push('Selected elements:\n' + ax.selected_elements.join('\n'));
    if (ax.document_text !== undefined) parts.push('Document text:\n' + ax.document_text);
    return parts.join('\n\n');
  }
  function windowTarget(computer, initialWindow, platform) {
    let window = initialWindow, screenshotId;
    const linux = platform === 'linux';
    async function capture(text, screenshot, options) {
      if (!linux && screenshot && options?.emit === false) throw new Error('Windows screenshots are displayed by Sky; emit: false is unavailable.');
      screenshotId = undefined;
      const state = await computer.get_window_state({window,...(linux ? {} : {include_text:text}),include_screenshot:screenshot});
      window = state.window;
      if (!linux && state.screenshots.length === 1) screenshotId = state.screenshots[0]?.id;
      return state;
    }
    const stateText = state => linux ? 'Accessibility source: ' + state.ax_tree_source + '\n' + state.ax_tree.to_string() : windowsState(state);
    const point = value => Array.isArray(value) ? {x:value[0],y:value[1],...(!linux && screenshotId !== undefined ? {screenshotId} : {})} : linux ? {element_id:String(value)} : {element_index:value};
    return {
      async getAXState(options) {const state=stateText(await capture(true,false,options));await emitState(state,options);return state;},
      async getScreenshot(options) {
        const shot=singleScreenshot((await capture(false,true,options)).screenshots,window.id);
        if (shot === undefined) throw new Error('Screenshot unavailable for window ' + window.id + '.');
        if (linux) {await emitImage(shot.bytes,options,'image/jpeg');return shot.bytes;}
        return imageFromUrl(shot.url);
      },
      async getAXStateAndScreenshot(options) {
        const captured=await capture(true,true,options),state=stateText(captured);await emitState(state,options);
        const shot=singleScreenshot(captured.screenshots,window.id);
        if (shot === undefined) return {state};
        const screenshot=linux ? shot.bytes : await imageFromUrl(shot.url);
        if (linux) await emitImage(screenshot,options,'image/jpeg');
        return {state,screenshot};
      },
      click(value,options) {return computer.click({window,...point(value),...(options?.mouseButton===undefined?{}:{mouse_button:options.mouseButton}),...(options?.clickCount===undefined?{}:{click_count:options.clickCount})});},
      drag(from,to) {return computer.drag({window,...(linux?{path:[{x:from[0],y:from[1]},{x:to[0],y:to[1]}]}:{from_x:from[0],from_y:from[1],to_x:to[0],to_y:to[1],...(screenshotId===undefined?{}:{screenshotId})})});},
      scroll(value,direction,distance) {
        if (linux) {
          if (typeof distance === 'number') throw new Error('Linux scroll accepts { pixels }, not pages.');
          return computer.scroll({window,direction,...point(value),...(distance===undefined?{}:{pixels:pixels(distance.pixels)})});
        }
        if (!Array.isArray(value) || typeof distance !== 'object') throw new Error('Windows scroll requires a point and a { pixels } distance.');
        const amount=pixels(distance.pixels);let scrollX=0,scrollY=0;
        switch(direction) {case 'u':case 'up':scrollY=-amount;break;case 'd':case 'down':scrollY=amount;break;case 'l':case 'left':scrollX=-amount;break;case 'r':case 'right':scrollX=amount;break;default:throw new Error('Unknown scroll direction: ' + direction + '.');}
        return computer.scroll({window,...point(value),scrollX,scrollY});
      },
      async selectText() {throw new Error('selectText is unavailable on ' + (linux?'Linux':'Windows') + '.');},
      setValue(index,value) {if(linux)return Promise.reject(new Error('setValue is unavailable on Linux; use click, pressKey, and typeText.'));return computer.set_value({window,element_index:index,value});},
      paste(text,options) {textPaste(options,linux?'Linux':'Windows');return computer.type_text({window,text});},
      pressKey(key) {return computer.press_key({window,key});},
      typeText(text) {return computer.type_text({window,text});},
      performSecondaryAction(index,action) {return computer.perform_secondary_action({window,...(linux?{element_id:String(index)}:{element_index:index}),action});}
    };
  }
  function parseTabMention(value) {
    const url=URL.parse(value);
    if(url?.protocol!=='plugin:' || url.host!=='openai-bundled' || !['browser','chrome','chrome-dev','chrome-internal'].includes(url.username) || url.password || (url.pathname!=='' && url.pathname!=='/') || url.hash) throw new Error('Invalid tab mention URL.');
    const fields=Object.fromEntries(url.searchParams),{mention,source,browserId,tabId,title,url:tabUrl}=fields;
    const kind=url.username==='browser' ? source ?? 'iab' : 'extension';
    if(mention!=='tab-v1' || url.searchParams.size!==Object.keys(fields).length || !['iab','extension'].includes(kind) || !browserId?.trim() || !tabId?.trim() || title===undefined || tabUrl===undefined) throw new Error('Invalid tab mention fields.');
    return {source:kind,browserId,tabId,title,url:tabUrl};
  }
  async function getApps(computer) {
    switch(computer.target) {
      case 'mac':case 'windows':return computer.list_apps();
      case 'linux':return (await computer.list_apps()).map(({id,name,windows})=>({id,displayName:name,isRunning:windows.length>0,windows}));
      default:{const error=new Error(computer.target);error.name='UnreachableCaseError';throw error;}
    }
  }
  const normalizeUrl = value => value === undefined || URL.canParse(value) ? value : 'https://' + value;
  const packagedDocumentation = name => {
    if (!Object.hasOwn(__skyreDocumentation,name)) {
      const error = new Error(`Documentation is not available: ${name}`);
      error.code = 'ENOENT';
      throw error;
    }
    return __skyreDocumentation[name];
  };
  async function createCUA({computer,browsers,readDocumentation = packagedDocumentation,getNodeRepl = () => globalThis.nodeRepl,getDocumentationContext = getNodeRepl}) {
    const documentation = new Map();
    let coreText, coreRequestMeta, otherBrowserSent = false, queue = Promise.resolve();
    const documentedBrowsers = new Map();
    const initialDoc = () => getDocumentationContext()?.env?.TINYSKY_ALT_INITIALIZE_DOCS ?? 'core-cua-repl';
    async function confirmationPolicy() {
      const metadata = getDocumentationContext()?.requestMeta?.['openai/confirmation_policies'];
      const value = metadata && typeof metadata === 'object' && !Array.isArray(metadata) ? metadata.computer_use : undefined;
      if (typeof value === 'string' && value.trim() !== '' && new TextEncoder().encode(value).length <= 12000) return value;
      return readDocumentation('confirmations');
    }
    function enqueue(operation) {
      const output = getNodeRepl();
      const writer = output?.write;
      const sink = writer == null ? undefined : writer.bind(output);
      if (sink === undefined) return Promise.resolve();
      const requestMeta = getDocumentationContext()?.requestMeta;
      const pending = queue.then(() => operation(sink,requestMeta));
      queue = pending.catch(() => {});
      return pending;
    }
    function emit(value = '', options) {
      return enqueue(async (sink,requestMeta) => {
        const browser = options?.browser;
        let core = '', shared = '', specific = '';
        if (coreText === undefined) {
          const name = initialDoc();
          core = await readDocumentation(name);
          if (name === 'core-cua-repl') core += '\n' + await confirmationPolicy();
        }
        if (browser !== undefined) {
          const value = await documentation.get(browser.browserId);
          if (!otherBrowserSent) shared = await readDocumentation('other-browser-apis');
          if (value !== undefined && !documentedBrowsers.has(browser.browserId)) specific = value;
        }
        const state = options?.emit === false ? '' : typeof value === 'string' ? value : JSON.stringify(value);
        if (core !== '') { await sink(core,'cua.core'); coreRequestMeta = requestMeta; }
        coreText ??= core;
        const browserText = [shared,specific].filter(value => value !== '').join('\n\n');
        if (browser !== undefined && browserText !== '') {
          await sink(browserText,'cua.browser.' + browser.browserId);
          documentedBrowsers.set(browser.browserId,{text:browserText,requestMeta});
        }
        if (browser !== undefined) otherBrowserSent = true;
        if (state !== '') await sink(state,'cua.state');
      });
    }
    function rewriteDocumentation() {
      return enqueue(async (sink,requestMeta) => {
        if (coreText && !(requestMeta != null && requestMeta === coreRequestMeta)) {
          await sink(coreText,'cua.core');
          coreRequestMeta = requestMeta;
        }
        for (const [id,record] of documentedBrowsers) {
          if (requestMeta != null && requestMeta === record.requestMeta) continue;
          await sink(record.text,'cua.browser.' + id);
          record.requestMeta = requestMeta;
        }
      });
    }
    async function browserTabs(browser) {
      const userTabs = browser.user?.openTabs ? browser.user.openTabs().catch(error => {globalThis.console?.error?.(error);return [];}) : Promise.resolve([]);
      const [opened,controlled] = await Promise.all([userTabs,browser.tabs.list()]);
      const tabs = new Map();
      for (const tab of opened) tabs.set(tab.id,tab);
      for (const tab of controlled) tabs.set(tab.id,tab);
      return [...tabs.values()];
    }
    await emit();
    const result = {
      rewriteDocumentation,
      async getState(options) {
        const appPromise = (async () => {
          if (computer === undefined) return [];
          return getApps(computer);
        })();
        const browserPromise = (async () => {
          if (browsers === undefined) return [];
          return Promise.all((await browsers.list()).map(async info => ({...info,tabs:await browserTabs(await browsers.get(info.id))})));
        })();
        const [apps,browserStates] = await Promise.allSettled([appPromise,browserPromise]);
        const errors = [];
        for (const [name,inventory] of Object.entries({'Native apps':apps,Browsers:browserStates})) {
          if (inventory.status === 'rejected') errors.push(name + ': ' + String(inventory.reason));
        }
        const state = {apps:apps.status === 'fulfilled' ? apps.value : [],browsers:browserStates.status === 'fulfilled' ? browserStates.value : [],...(errors.length ? {errors} : {})};
        await emit(state,options);
        return state;
      }
    };
    if (browsers !== undefined) {
      async function choose(options,url) {
        let browser;
        if (options?.browser !== undefined) browser = await browsers.get(options.browser);
        else if (url !== undefined && browsers.getForUrl !== undefined) browser = await browsers.getForUrl(url);
        else if (browsers.getDefault !== undefined) browser = await browsers.getDefault();
        else {
          const first = (await browsers.list())[0];
          if (first === undefined) throw new Error('No browser is available.');
          browser = await browsers.get(first.id);
        }
        if (getNodeRepl()?.write !== undefined) {
          let pending = documentation.get(browser.browserId);
          if (pending === undefined) {
            pending = browser.documentation().catch(error => {documentation.delete(browser.browserId);throw error;});
            documentation.set(browser.browserId,pending);
          }
          await pending;
        }
        return browser;
      }
      async function displayInitial(tab,browser) {
        const state = await tab.getAXState({disableDiffing:true,emit:false});
        await emit(state,{browser});
        return tab;
      }
      // Publish before assembling the temporary browser API, as in the original
      // factory. A throwing setter prevents assembly; Reflect.set(false) is ignored.
      Reflect.set(globalThis,'agent',{browsers,documentation:{get:async name=>rpc('browser.documentation',{name})}});
      Object.assign(result,{
        browsers,
        async getBrowser(options) {
          let id=options?.id;
          if(options?.extensionInstanceId!==undefined) {
            if(id!==undefined) throw new Error('Specify either id or extensionInstanceId, not both.');
            const matches=(await browsers.list()).filter(info=>info.type==='extension' && info.metadata?.extensionInstanceId===options.extensionInstanceId);
            if(matches.length===0) throw new Error('The Chrome instance is unavailable.');
            if(matches.length!==1) throw new Error('Multiple browsers match the Chrome instance: ' + JSON.stringify(matches));
            id=matches[0].id;
          }
          const url = normalizeUrl(options?.url);
          const browser = await choose({browser:id},url);
          await emit(undefined,{browser});
          return browser;
        },
        async createBrowserTab(id,url,options) {
          if (typeof id !== 'string' || id.trim() === '') throw new Error('createBrowserTab requires a browser ID. Select one with cua.getBrowser().');
          const normalized = normalizeUrl(url);
          const browser = await choose({browser:id});
          if (options?.sessionName !== undefined) {
            if (typeof browser.nameSession !== 'function') throw new Error('Browser ' + browser.browserId + ' does not support sessionName.');
            await browser.nameSession(options.sessionName);
          }
          if (options?.visible !== undefined) await (await browser.capabilities.get('visibility')).set(options.visible);
          const tab = await browser.tabs.new();
          if (normalized !== undefined) await tab.goto(normalized);
          return displayInitial(tab,browser);
        },
        async getTab(reference,options) {
          let browser,controlled,matches;
          async function matching(predicate) {
            const direct=controlled.filter(predicate);
            if(direct.length) return direct;
            const user=await browser.user?.openTabs?.() ?? [];
            const providerIds=new Map(user.map(tab=>[tab.id,tab.providerTabId]));
            return [...new Map([...user,...controlled.map(tab=>({...tab,providerTabId:tab.providerTabId??providerIds.get(tab.id)}))].map(tab=>[tab.id,tab])).values()].filter(predicate);
          }
          if(typeof reference==='string') {
            if(reference==='') throw new Error('getTab requires a tab reference');
            browser=await choose(options);controlled=await browser.tabs.list();
            matches=await matching(tab=>tab.id===reference || tab.providerTabId===reference);
          } else if('mention' in reference) {
            const mention=parseTabMention(reference.mention);
            const infos=(await browsers.list()).filter(info=>info.type===mention.source && (mention.source==='iab' || info.metadata?.extensionInstanceId===mention.browserId));
            if(infos.length!==1) throw new Error(infos.length===0?"The browser/profile referenced by the tab mention is unavailable.":"Multiple browsers match the tab mention's browser/profile: " + JSON.stringify(infos));
            browser=await choose({browser:infos[0].id});
            if(options?.browser!==undefined && (await browsers.get(options.browser)).browserId!==browser.browserId) throw new Error("The requested browser does not match the tab mention's browser/profile.");
            controlled=await browser.tabs.list();matches=await matching(tab=>tab.providerTabId===mention.tabId);
            if(matches[0]!==undefined && (matches[0].title!==mention.title || matches[0].url!==mention.url)) throw new Error("Stale tab mention: the tab's title or URL has changed.");
          } else {
            if(!URL.canParse(reference.url)) throw new Error('getTab requires an absolute URL.');
            if(!options?.browser) throw new Error('getTab({ url }) requires an explicit browser.');
            browser=await choose(options);controlled=await browser.tabs.list();
            const user=await browser.user?.openTabs?.() ?? [];
            matches=[...new Map([...user,...controlled].map(tab=>[tab.id,tab])).values()].filter(tab=>tab.url===reference.url);
          }
          const match=matches[0];
          if(match===undefined) throw new Error('Tab not found in browser ' + browser.browserId + '.');
          if(matches.length!==1) throw new Error('Multiple tabs match the reference in browser ' + browser.browserId + ': ' + JSON.stringify(matches.map(tab=>({...tab,browserId:browser.browserId}))));
          let tab;
          if(controlled.some(item=>item.id===match.id)) tab=await browser.tabs.get(match.id);
          else {
            if(browser.user?.claimTab===undefined) throw new Error('Tab ' + match.id + ' cannot be claimed in browser ' + browser.browserId + '.');
            tab=await browser.user.claimTab(match);
          }
          return displayInitial(tab,browser);
        },
        async listBrowsers(options) { const list = await browsers.list();await emit(list,options);return list; },
        async listTabs(options) {
          let selected;
          const infos = options?.browser === undefined ? await browsers.list() : [{id:options.browser}];
          const tabs = (await Promise.all(infos.map(async info => {
            let browser;
            if (options?.browser !== undefined) { browser = await choose(options); selected = browser; }
            else browser = await browsers.get(info.id);
            return (await browserTabs(browser)).map(tab => ({...tab,browserId:browser.browserId}));
          }))).flat();
          await emit(tabs,{...options,browser:selected});
          return tabs;
        }
      });
    }
    if (computer !== undefined) {
      Object.assign(result,{computer,async listApps(options) {const apps=await getApps(computer);await emit(apps,options);return apps;}});
      const platform=computer.target;
      if(platform==='mac') Object.assign(result,{
        async getApp(identifier) {
          if(typeof identifier!=='string') throw new Error('macOS getApp requires an app name, path, or bundle ID.');
          const state=await computer.get_app_state({app:identifier,disableDiff:true});
          const app=nativeTarget(computer,state.app);await emit(state.text);return app;
        }
      });
      else if(platform==='linux' || platform==='windows') Object.assign(result,{
        async getApp(reference) {
          const id=windowId(reference,platform);
          const window=platform==='linux' ? (await computer.list_windows()).find(window=>window.id===id) : await computer.get_window({id});
          if(platform==='linux' && window===undefined) throw new Error('Window ' + id + ' is unavailable.');
          const app=windowTarget(computer,window,platform),state=await app.getAXState({disableDiffing:true,emit:false});
          await emit(state);return app;
        },
        async listWindows(options) {const windows=await computer.list_windows();await emit(windows,options);return windows;}
      });
      else {const error=new Error(platform);error.name='UnreachableCaseError';throw error;}
    }
    return result;
  }
  // Testable dependency boundary; production supplies only Rust-backed adapters.
  Object.defineProperty(globalThis,'__skyreCreateCUA',{value:createCUA});
  let setup;
  const cua = {async initialize() {await initialize();return cua.getState();}};
  function initialize(options = {}) {
    if (setup == null) setup = (async () => {
      // Trusted launcher errors still reject before option reads or factories.
      if (setupSurfaces.error !== null) throw new Error(setupSurfaces.error);
      // Read browser first and enqueue its construction before reading computer.
      // Actual Node provider imports remain outside this native adapter owner.
      const browserSetup = options.browser !== false ? Promise.resolve().then(() => {
        const provider = browserFacade({rpc,trackOperation,deriveOperation,ownedYield,target:()=>({}),bytes,emitImage:value=>globalThis.nodeRepl.emitImage({bytes:value,mimeType:'image/png'}),emitBrowserDocumentation:async id=>rpc('browser.documentation',{browser:id})});
        const wrap = info => provider.browser(info.id,info);
        return {value:{
          async list() {return (await rpc('browser.list')).map(info=>Object.fromEntries(
            ['id','name','family','type','profileName','metadata'].filter(key=>Object.hasOwn(info,key)).map(key=>[key,info[key]])));},
          async get(id) {return wrap(await rpc('browser.info',{browser:id}));},
          async getDefault() {return wrap(await rpc('browser.get_default_browser'));},
          async getForUrl(url) {return wrap(await rpc('browser.get_browser_for_url',{url}));}
        }};
      }) : undefined;
      let computerSetup;
      try {
        computerSetup = options.computer !== false ? Promise.resolve().then(async () => {
          let value, error;
          try { value = await rpc('sky.setup'); } catch(failure) { error = failure; }
          // A failed native computer adapter has a synthetic then property.
          // Keep it boxed so setup does not assimilate that error proxy.
          return {value:__skyreComputerFacade({rpc,bytes,getNodeRepl:getPrivateNodeRepl,setupResult:{value,error}})};
        }) : undefined;
      } catch (error) {
        // Preserve the primary option error while observing an already-enqueued
        // native construction. This does not recreate fatal proxy/import bugs.
        if (browserSetup !== undefined) browserSetup.catch(() => {});
        throw error;
      }
      const [browserResult, computerResult] = await Promise.all([browserSetup,computerSetup]);
      const browsers = browserResult?.value, computer = computerResult?.value;
      const api = await createCUA({computer,browsers,getDocumentationContext:getPrivateNodeRepl});
      return api;
    })().then(api => {Object.assign(cua,api,{initialize:api.getState});});
    return setup;
  }
  globalThis.cua = cua;
  Object.defineProperty(globalThis,'__skyreInitialize',{value:initialize});
  globalThis.skyre = {
    capabilities:async()=>rpc('capabilities'),
    control:{
      async acquire(ttlMs=30000) {const receipt=await rpc('guardian.acquire',{ttl_ms:ttlMs});guardianLease=receipt.lease;return receipt;},
      async renew(ttlMs=30000) {if(!guardianLease)throw new Error('No control lease is held');const receipt=await rpc('guardian.renew',{lease:guardianLease,ttl_ms:ttlMs});guardianLease=receipt.lease;return receipt;},
      async release() {if(!guardianLease)return {released:false};const result=await rpc('guardian.release',{lease:guardianLease});guardianLease=undefined;return result;},
      async status() {return rpc('guardian.status');}
    }
  };
})();
