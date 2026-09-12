// Independently implemented compatibility boundary for the installed embedded Sky API.
// All observation and input is delegated to the Rust host; no vendor code is loaded.
globalThis.__skyreComputerFacade = ({rpc, bytes, getNodeRepl = () => globalThis.nodeRepl, setupResult}) => {
  let setup, setupError;
  if(setupResult) { setup=setupResult.value; setupError=setupResult.error; }
  else { try { setup = rpc("sky.setup"); } catch (error) { setupError = error; } }
  const computer = {target: setup?.target};
  let linuxAudioActive=false, linuxAudioStarting=false;
  // Snapshot own data properties before policy/lowering; never evaluate getters.
  const appInput = (input, canonical) => {
    if (typeof input !== "object" || input === null) throw new Error("Computer Use app approval requires an object input");
    const properties = Object.getOwnPropertyDescriptors(input);
    if (properties.app === undefined || !("value" in properties.app)) {
      throw new Error("Computer Use app approval requires app to be a plain data property");
    }
    if (typeof properties.app.value !== "string" || properties.app.value.trim() === "") {
      throw new Error("Computer Use app approval requires a non-empty app");
    }
    const snapshot = {};
    for (const [name,property] of Object.entries(properties)) {
      if (!("value" in property)) throw new Error("Computer Use app approval requires " + name + " to be a plain data property");
      Object.defineProperty(snapshot,name,{enumerable:property.enumerable,value:name === "app" ? canonical ?? property.value : property.value,writable:false,configurable:false});
    }
    return Object.freeze(snapshot);
  };
  const responseMeta = app => {
    getNodeRepl()?.setResponseMeta?.({
      "codex/toolSurface":{app:app == null ? null : {appId:app,kind:"appId"},kind:"computerUse"},
      ...(app === "com.google.Chrome" ? {"codex/computerUseChrome":true} : {}),
    });
  };
  const callback = name => {
    const fn = getNodeRepl()?.[name];
    if (typeof fn !== "function") throw new Error("Computer Use requires nodeRepl." + name);
    return fn;
  };
  const callMetadata = () => {
    let meta = getNodeRepl()?.requestMeta?.["x-codex-turn-metadata"];
    if (typeof meta === "string") {try {meta=JSON.parse(meta);} catch {return {};}}
    if (meta && typeof meta === "object" && !Array.isArray(meta)) {
      for (const field of ["call_id","item_id"]) {
        if (typeof meta[field] === "string" && meta[field].trim()) return {tool_call_id:meta[field].trim()};
      }
    }
    return {};
  };
  const withAppPolicy = async (method, raw, operation) => {
    responseMeta(null);
    const input=appInput(raw);
    const elicit=callback("createElicitation"), suspended=callback("withSuspendedTimeout");
    const policy=await rpc("sky.app_policy",{app:input.app});
    const target=policy.target;
    responseMeta(target.bundleIdentifier);
    if (policy.decision === "denied") throw new Error("Computer Use is blocked from using the app '" + target.bundleIdentifier + "' by your organization's policy.");
    if (policy.decision === "forbidden") throw new Error("Computer Use is not allowed to use the app '" + target.bundleIdentifier + "' for safety reasons.");
    if (policy.decision !== "allowed") throw new Error("Invalid computer-use app policy decision");
    const approval=await elicit({message:'Allow Computer Use to use "' + target.displayName + '"?',meta:{
      codex_approval_kind:"mcp_tool_call",connector_id:"computer-use",connector_name:"Computer Use",
      persist:policy.allowPersistentApproval?["session","always"]:["session"],riskLevel:target.risk,
      ...(target.warningSubtitle == null ? {} : {subtitle:target.warningSubtitle}),...callMetadata(),
      tool_name:method,tool_params:{app:target.bundleIdentifier},
      tool_params_display:[{name:"app",display_name:"App",value:target.displayName}],
    }});
    if (approval.action !== "accept") throw new Error("Computer Use was not approved to use " + target.displayName);
    return suspended(()=>operation(appInput(input,target.appPath)));
  };
  const audioApproval = async () => {
    const approval=await callback("createElicitation")({message:"Allow Computer Use to record computer audio?",meta:{
      codex_approval_kind:"mcp_tool_call",codex_request_type:"approval_request",connector_id:"computer-use",connector_name:"Computer Use",
      persist:["session"],riskLevel:"high",...callMetadata(),tool_name:"start_audio_recording",tool_params:{},tool_params_display:[],
    }});
    if (approval.action !== "accept") throw new Error("Computer Use was not approved to record computer audio");
  };
  const execute = async (method, args) => {
    const value = await rpc("sky.execute", {method, args});
    return value == null ? undefined : value;
  };
  const requireApp = app => {
    if (app == null || app.trim() === "") throw new TypeError("app is required");
    return app;
  };
  const element = value => {
    if (!Number.isInteger(value)) throw new TypeError("elementIndex must be an integer");
    return value;
  };
  const point = (x, y, label) => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      throw new TypeError(`${label} must include finite x and y coordinates`);
    }
    return [Number(x), Number(y)];
  };
  const target = input => {
    if (input.element_index != null) return {element_index: element(input.element_index)};
    const [x,y] = point(input.x,input.y,"coordinate");
    return {x,y};
  };
  const mouse = button => {
    if (typeof button === "number") {
      if ([0,1,2].includes(button)) return button;
      throw new TypeError("mouseButton number must be 0, 1, or 2");
    }
    const value = button.trim().toLowerCase();
    if (value === "left" || value === "l") return 0;
    if (value === "right" || value === "r") return 1;
    if (value === "middle" || value === "m") return 2;
    throw new TypeError("mouseButton must be left, right, middle, l, r, m, 0, 1, or 2");
  };
  const direction = value => {
    switch (value.trim().toLowerCase()) {
      case "u": case "up": return "up";
      case "d": case "down": return "down";
      case "l": case "left": return "left";
      case "r": case "right": return "right";
      default: throw new TypeError("direction must be up, down, left, or right");
    }
  };
  const media = value => ({
    filepath: value.filepath,
    // Captured Sky hydrates with Buffer.from(payload, 'base64'): accept the
    // URL alphabet, ignore non-alphabet characters, and stop at first padding.
    // UTF-16 code units are truncated to bytes before alphabet/padding checks.
    bytes: bytes({data: (value.data_url.split(",")[1] ?? "").replace(/[\s\S]/g, char => String.fromCharCode(char.charCodeAt(0) & 255)).split("=")[0]
      .replace(/[^A-Za-z0-9+/_-]/g, "").replace(/-/g, "+").replace(/_/g, "/")}),
    data_url: value.data_url,
  });
  async function mac(method, args) {
    const input = method === "list_apps" ? undefined : appInput(args[0]);
    if (method === "list_apps") {
      return (await execute(method, [])).map(app => ({
        id:app.bundleIdentifier ?? app.displayName ?? "unknown",
        displayName:app.displayName,isRunning:app.isRunning,
        lastUsedDate:app.lastUsedDate ?? undefined,useCount:app.useCount ?? undefined,
      }));
    }
    if (method === "get_app_state") {
      const request = {app:requireApp(input.app),disableDiff:input.disableDiff};
      return await execute(method,[request]);
    }
    let request;
    switch (method) {
      case "click": {
        const count = input.click_count === undefined ? 1 : input.click_count;
        const button = input.mouse_button === undefined ? "left" : input.mouse_button;
        const position = target(input);
        request = {...position,mouse_button:mouse(button),click_count:count};
        break;
      }
      case "drag": {
        const from = point(input.from_x,input.from_y,"from");
        const to = point(input.to_x,input.to_y,"to");
        request = {from_x:from[0],from_y:from[1],to_x:to[0],to_y:to[1]};
        break;
      }
      case "press_key": {
        // Preserve the installed client's receiver expression in engine-created
        // TypeError messages, including non-string values with no trim method.
        const e = input;
        if (e.key.trim() === "") throw new TypeError("key is required");
        request = {key:e.key};
        break;
      }
      case "scroll": {
        const pages = input.pages === undefined ? 1 : input.pages;
        if (!Number.isFinite(pages) || pages <= 0) throw new TypeError("pages must be a finite number > 0");
        request = {...target(input),direction:direction(input.direction),pages};
        break;
      }
      case "select_text":
        request = {element_index:element(input.element_index),text:input.text,prefix:input.prefix,suffix:input.suffix,
          selection_type:input.selection_type === undefined ? "text" : input.selection_type};
        break;
      case "set_value": request = {element_index:element(input.element_index),value:input.value}; break;
      case "perform_secondary_action": request = {element_index:element(input.element_index),action:input.action}; break;
      case "type_text": request = {text:input.text}; break;
      case "paste": request = {text:input.text,format:input.format}; break;
      default: return execute(method,args);
    }
    request.app = requireApp(input.app);
    await execute(method,[request]);
  }
  // Window2 uses the Windows helper's normalization and approval protocol,
  // independently of the Mac wrapper and full-desktop Linux interface.
  const nonempty = value => typeof value === "string" && value.trim() !== "" ? value : undefined;
  const windowId = value => {
    if (typeof value === "string" && value.trim()) value=Number(value);
    return Number.isInteger(value) && value >= 0 ? value : undefined;
  };
  const normalizeWindow = (value, fallback) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return;
    const id=windowId(value.id), app=nonempty(value.app) ?? fallback;
    if (id === undefined || app === undefined) return;
    return {app,id,...(typeof value.title === "string" ? {title:value.title} : {})};
  };
  const requiredWindow = value => {
    const window=normalizeWindow(value);
    if (!window) throw new TypeError("window.app must be a non-empty string and window.id must be an integer >= 0");
    return window;
  };
  const normalizeWindows = (value, fallback) => Array.isArray(value) ? value.flatMap(row=>{
    const window=normalizeWindow(row,fallback);return window ? [window] : [];
  }) : [];
  const rounded = (value,label) => {
    if (typeof value !== "number" || !Number.isFinite(value)) throw new TypeError(label + " must be a finite number");
    return Math.round(value);
  };
  const windowsElement = value => {
    const index=windowId(value);
    if (index === undefined) throw new TypeError("element_index must be an integer >= 0");
    return index;
  };
  const windowsState = (value, requested, includeScreenshot) => {
    const invalid = suffix => {throw new Error("codex-computer-use.exe did not return " + suffix);};
    if (!value || typeof value !== "object") invalid("window state");
    let accessibility=null;
    if (value.accessibility != null) {
      const state=value.accessibility;
      if (typeof state !== "object" || Array.isArray(state)) invalid("accessibility state");
      if (typeof state.tree !== "string") invalid("accessibility tree");
      if (state.selected_elements != null && (!Array.isArray(state.selected_elements) || state.selected_elements.some(item=>typeof item !== "string"))) invalid("selected_elements");
      accessibility={tree:state.tree};
      for (const field of ["focused_element","selected_text"]) if (typeof state[field] === "string") accessibility[field]=state[field];
      if (state.selected_elements != null) accessibility.selected_elements=state.selected_elements;
      if (typeof state.document_text === "string") accessibility.document_text=state.document_text;
    }
    if (value.screenshots == null ? includeScreenshot : !Array.isArray(value.screenshots)) invalid("screenshots");
    const screenshots=(value.screenshots ?? []).map(shot=>{
      if (!shot || typeof shot !== "object" || Array.isArray(shot)) invalid("a screenshot");
      if (typeof shot.url !== "string" || !shot.url.length) invalid("a screenshot URL");
      const out={id:typeof shot.id === "string" ? shot.id : "screenshot-0",zIndex:Number.isFinite(shot.zIndex)?shot.zIndex:0,url:shot.url};
      for (const field of ["originX","originY","width","height"]) if (Number.isFinite(shot[field])) out[field]=shot[field];
      return out;
    });
    return {window:normalizeWindow(value.window,requested.app) ?? requested,screenshots,accessibility};
  };
  const windowsApproval = async (method,params) => {
    const audio=method === "start_audio_recording";
    const policy=audio ? {allowPersistentApproval:false,target:{bundleIdentifier:"computer-audio",displayName:"Computer audio",risk:"high"}}
      : await rpc("sky.windows_policy",{method,params});
    if (policy.approvalRequired === false) return;
    const target=policy.target;
    if (policy.decision === "denied" || policy.decision === "forbidden") throw new Error("Application denied by configured policy");
    const elicit=getNodeRepl()?.createElicitation;
    if (typeof elicit !== "function") throw new Error("Computer Use requires app approval but elicitations are unavailable");
    const persistent=!!policy.allowPersistentApproval && !audio;
    const approval=await elicit({message:audio ? "Allow Computer Use to record computer audio?" : "Allow Codex to use " + target.displayName + "?",meta:{
      codex_approval_kind:"mcp_tool_call",...(audio?{codex_request_type:"approval_request"}:{}),
      connector_id:"computer-use",connector_name:"Computer Use",persist:persistent?["session","always"]:["session"],riskLevel:target.risk ?? "low",
      ...(audio?{...callMetadata(),tool_name:"start_audio_recording"}:{}),
      tool_params:{app:target.bundleIdentifier},tool_params_display:[{name:"app",display_name:"App",value:target.displayName}],
    }});
    const globalPersist=approval?._meta?.persist === "always" || approval?.content?.persist === "always" ||
      (approval?.content?.source === "computer-use-persisted-state" && approval?.content?.scope === "global");
    if (approval.action !== "accept" || (!persistent && globalPersist)) throw new Error("Computer Use was not approved to use " + target.displayName);
  };
  const windowsRequest = async (method,params) => {
    const operation=async()=>{
      if (!["list_apps","list_windows","stop_audio_recording"].includes(method)) await windowsApproval(method,params);
      return execute(method,[params]);
    };
    const suspended=getNodeRepl()?.withSuspendedTimeout;
    return typeof suspended === "function" ? suspended(operation) : operation();
  };
  async function windows(method,args) {
    let input=args[0];
    if (method === "list_windows") return normalizeWindows(await windowsRequest(method,{}));
    if (method === "list_apps") {
      const apps=await windowsRequest(method,{});
      return Array.isArray(apps)?apps.flatMap(row=>{
        if (!row || typeof row !== "object" || !nonempty(row.id)) return [];
        const app={id:row.id,windows:normalizeWindows(row.windows,row.id)};
        for (const field of ["displayName","lastUsedDate"]) if (typeof row[field] === "string") app[field]=row[field];
        if (Number.isFinite(row.useCount)) app.useCount=row.useCount;
        if (typeof row.isRunning === "boolean") app.isRunning=row.isRunning;
        return [app];
      }):[];
    }
    if (method === "get_window") {
      if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("get_window input must be an object");
      const id=windowId(input.id);
      if (id === undefined) throw new TypeError("id must be an integer >= 0");
      const app=nonempty(input.app), request=app === undefined ? {id} : {app,id};
      const window=normalizeWindow(await windowsRequest(method,request));
      if (!window) throw new Error("codex-computer-use.exe did not return a window");
      return window;
    }
    if (method === "launch_app") {
      const app=input && typeof input === "object" ? nonempty(input.app) : undefined;
      if (app === undefined) throw new TypeError("app is required");
      await windowsRequest(method,{app});return;
    }
    if (method === "start_audio_recording") {
      await windowsRequest(method,{max_duration_ms:(input === undefined ? {} : input).max_duration_ms});return;
    }
    if (method === "stop_audio_recording") {
      const result=await windowsRequest(method,{});
      if (!result || typeof result !== "object") throw new Error("codex-computer-use.exe did not return computer audio");
      if (typeof result.filepath !== "string" || !result.filepath.length) throw new Error("codex-computer-use.exe did not return a computer audio filepath");
      return media(result);
    }
    const rawInput=input;
    const fields={activate_window:["window"],get_window_state:["include_screenshot","include_text","window"],
      click:["window","click_count","element_index","mouse_button","screenshotId","x","y"],
      scroll:["scrollX","scrollY","screenshotId","window","x","y"],drag:["window","from_x","from_y","to_x","to_y","screenshotId"],
      press_key:["window","key"],type_text:["window","text"],set_value:["window","element_index","value"],perform_secondary_action:["window","action","element_index"]}[method];
    input={};for (const field of fields) input[field]=rawInput[field];
    // These checks intentionally precede window validation, matching the client.
    if (method === "press_key" && !input.key) throw new TypeError("key is required");
    if (method === "type_text" && typeof input.text !== "string") throw new TypeError("text is required");
    if (method === "set_value" && typeof input.value !== "string") throw new TypeError("value is required");
    if (method === "perform_secondary_action" && (typeof input.action !== "string" || !input.action.trim())) throw new TypeError("action is required");
    const include_screenshot=input.include_screenshot === undefined ? true : input.include_screenshot;
    const include_text=input.include_text === undefined ? false : input.include_text;
    if (method === "get_window_state" && !include_screenshot && !include_text) throw new TypeError("get_window_state must request include_text, include_screenshot, or both");
    const window=requiredWindow(input.window);
    let request={window};
    const shot=()=>input.screenshotId == null ? {} : {screenshotId:input.screenshotId};
    switch (method) {
      case "get_window_state": {
        const state=windowsState(await windowsRequest(method,{window,include_screenshot,include_text}),window,include_screenshot);
        for (const screenshot of state.screenshots) {
          if (getNodeRepl()?.emitImage != null) await getNodeRepl().emitImage(screenshot.url);
          else if (globalThis.codex?.emitImage != null) await globalThis.codex.emitImage({type:"input_image",image_url:screenshot.url,detail:"original"});
        }
        return state;
      }
      case "click": {
        const index=input.element_index ?? rawInput.elementIndex ?? rawInput.element;
        if (index == null) {
          if (input.x == null || input.y == null) throw new TypeError("click requires either element_index or finite x and y coordinates");
          request={window,x:rounded(input.x,"point.x"),y:rounded(input.y,"point.y"),...shot()};
        } else request.element_index=windowsElement(index);
        const count=rounded(input.click_count === undefined ? 1 : input.click_count,"click_count");
        if (count < 1) throw new TypeError("click_count must be >= 1");
        request.click_count=count;request.mouse_button=input.mouse_button === undefined ? "left" : input.mouse_button;break;
      }
      case "scroll": request={window,x:rounded(input.x,"scroll.x"),y:rounded(input.y,"scroll.y"),...shot(),scrollX:rounded(input.scrollX,"scroll.scrollX"),scrollY:rounded(input.scrollY,"scroll.scrollY")};break;
      case "drag": request={window,from_x:rounded(input.from_x,"from.x"),from_y:rounded(input.from_y,"from.y"),to_x:rounded(input.to_x,"to.x"),to_y:rounded(input.to_y,"to.y"),...shot()};break;
      case "press_key": {
        const keys=input.key.split("+").map(key=>key.trim()).filter(Boolean);
        if (!keys.length) throw new TypeError("key is required");
        request.key=keys.join("+");break;
      }
      case "type_text": request.text=input.text;break;
      case "set_value": request.element_index=windowsElement(input.element_index);request.value=input.value;break;
      case "perform_secondary_action": request.element_index=windowsElement(input.element_index);request.action=input.action;break;
    }
    await windowsRequest(method,request);
  }
  for (const method of setup?.methods ?? []) {
    computer[method] = async (...args) => {
      if (computer.target === "windows") return windows(method,args);
      if (method === "start_audio_recording") {
        if (computer.target === "linux" && (linuxAudioActive || linuxAudioStarting)) throw new Error("computer audio recording is already active");
        if (computer.target === "mac") responseMeta(null);
        const input = args[0] === undefined ? {} : args[0];
        const duration = input.max_duration_ms ?? 60000;
        if (!Number.isInteger(duration) || duration < 100 || duration > 300000) {
          throw new Error((computer.target === "linux" ? "max_duration_ms" : "audio recording duration") + " must be an integer from 100 through 300000");
        }
        if (computer.target === "mac") await audioApproval();
        if (computer.target === "linux") {
          linuxAudioStarting=true;
          try {await execute(method,[{max_duration_ms:duration}]);linuxAudioActive=true;}
          finally {linuxAudioStarting=false;}
        } else await execute(method,[{max_duration_ms:duration}]);
        return;
      }
      if (method === "stop_audio_recording") {
        if (computer.target === "linux") {
          if (!linuxAudioActive) throw new Error("computer audio recording is not active");
          linuxAudioActive=false;
          return media(await execute(method,[]));
        }
        responseMeta(null);
        const operation=async()=>media(await execute(method,[]));
        const suspended=getNodeRepl()?.withSuspendedTimeout;
        return typeof suspended === "function" ? suspended(operation) : operation();
      }
      if (computer.target === "mac") {
        if (method === "list_apps") {responseMeta(null);return mac(method,args);}
        return withAppPolicy(method,args[0],input=>mac(method,[input,...args.slice(1)]));
      }
      const result = await execute(method,args);
      if (method === "get_screenshot") return result.map(media);
      if (["list_apps","list_windows","get_window","get_window_state"].includes(method)) return result;
    };
  }
  if (computer.target === "linux" && setup.methods.includes("drag_handle")) {
    let nextHandle = 0;
    computer.drag_handle = () => {
      // IDs are local to this host session; Rust retains independent handle state.
      const handle_id = `skyre-drag-${++nextHandle}`;
      return {
        async start(point) { await rpc("sky.drag_start",{handle_id,point}); },
        async move_to(point) { await rpc("sky.drag_move",{handle_id,point}); },
        async end() { await rpc("sky.drag_end",{handle_id}); },
      };
    };
  }
  return new Proxy(computer, {
    get(object,key) {
      const value = Reflect.get(object,key);
      if (typeof value === "function") return value.bind(object);
      if (key in object || setupError === undefined) return value;
      return () => Promise.reject(setupError);
    },
  });
};
