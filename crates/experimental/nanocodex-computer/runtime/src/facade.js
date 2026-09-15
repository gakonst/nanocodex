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
  async function emitImage(value, options) {
    if (options?.emit !== false) await globalThis.nodeRepl?.emitImage?.(value);
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
        if (result.screenshot === null) throw new Error('Screenshot unavailable for ' + app + '.');
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
    let coreSent = false, otherBrowserSent = false, queue = Promise.resolve();
    const documentedBrowsers = new Set();
    const initialDoc = () => getDocumentationContext()?.env?.TINYSKY_ALT_INITIALIZE_DOCS ?? 'core-cua-repl';
    async function confirmationPolicy() {
      const metadata = getDocumentationContext()?.requestMeta?.['openai/confirmation_policies'];
      const value = metadata && typeof metadata === 'object' && !Array.isArray(metadata) ? metadata.computer_use : undefined;
      if (typeof value === 'string' && value.trim() !== '' && new TextEncoder().encode(value).length <= 12000) return value;
      return readDocumentation('confirmations');
    }
    function emit(value = '', options) {
      const output = getNodeRepl();
      const writer = output?.write;
      const sink = writer == null ? undefined : writer.bind(output);
      if (sink === undefined) return Promise.resolve();
      const pending = queue.then(async () => {
        const browser = options?.browser;
        let core = '', shared = '', specific = '';
        if (!coreSent) {
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
        if (core !== '') await sink(core,'cua.core');
        coreSent = true;
        const browserText = [shared,specific].filter(value => value !== '').join('\n\n');
        if (browserText !== '') await sink(browserText,'cua.browser');
        if (browser !== undefined) { otherBrowserSent = true; documentedBrowsers.add(browser.browserId); }
        if (state !== '') await sink(state,'cua.state');
      });
      queue = pending.catch(() => {});
      return pending;
    }
    await emit();
    const result = {
      async getState(options) {
        const appPromise = !computer || computer.target === 'linux' ? [] : computer.list_apps();
        const [apps, infos] = await Promise.all([appPromise,browsers?.list() ?? []]);
        const states = browsers ? await Promise.all(infos.map(async info => {
          const browser = await browsers.get(info.id);
          const userTabs = browser.user?.openTabs ? browser.user.openTabs().catch(error => {globalThis.console?.error?.(error);return [];}) : [];
          const [opened,controlled] = await Promise.all([userTabs,browser.tabs.list()]);
          const tabs = new Map();
          for (const tab of opened) tabs.set(tab.id,tab);
          for (const tab of controlled) tabs.set(tab.id,tab);
          return {...info,tabs:[...tabs.values()]};
        })) : [];
        const state = {apps,browsers:states};
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
          const url = normalizeUrl(options?.url);
          const browser = await choose({browser:options?.id},url);
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
        async getTab(id,options) {
          if (!id) throw new Error('getTab requires a tab id');
          const browser = await choose(options);
          const tabs = await browser.tabs.list();
          const existing = tabs.find(tab => tab.id === id || tab.providerTabId === id);
          if (existing !== undefined) return displayInitial(await browser.tabs.get(existing.id),browser);
          if (browser.user?.openTabs !== undefined && browser.user.claimTab !== undefined) {
            const user = (await browser.user.openTabs()).find(tab => tab.id === id || tab.providerTabId === id);
            if (user !== undefined) return displayInitial(await (tabs.some(tab => tab.id === user.id) ? browser.tabs.get(user.id) : browser.user.claimTab(user)),browser);
          }
          throw new Error('Tab not found: ' + id + ' in browser ' + browser.browserId);
        },
        async listBrowsers(options) { const list = await browsers.list();await emit(list,options);return list; },
        async listTabs(options) {
          let selected;
          const infos = options?.browser === undefined ? await browsers.list() : [{id:options.browser}];
          const tabs = (await Promise.all(infos.map(async info => {
            let browser;
            if (options?.browser !== undefined) { browser = await choose(options); selected = browser; }
            else browser = await browsers.get(info.id);
            return (await browser.tabs.list()).map(tab => ({...tab,browserId:browser.browserId}));
          }))).flat();
          await emit(tabs,{...options,browser:selected});
          return tabs;
        }
      });
    }
    if (computer !== undefined) Object.assign(result,{
      computer,
      async getApp(identifier) {
        if (computer.target !== 'mac') throw new Error('Native app bindings are unavailable for ' + computer.target + '.');
        const state = await computer.get_app_state({app:identifier,disableDiff:true});
        const app = nativeTarget(computer,state.app);
        await emit(state.text);
        return app;
      },
      async listApps(options) {
        if (computer.target !== 'mac') throw new Error('Native app bindings are unavailable for ' + computer.target + '.');
        const apps = await computer.list_apps();await emit(apps,options);return apps;
      }
    });
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
        const provider = browserFacade({rpc,trackOperation,deriveOperation,ownedYield,target:()=>({}),bytes,emitImage:value=>globalThis.nodeRepl.emitImage(value),emitBrowserDocumentation:async id=>rpc('browser.documentation',{browser:id})});
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
    })().then(api => {Object.assign(cua,api);});
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
