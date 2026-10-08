import { injectNativeVaultFields, parseNativeVaultInjection, injectBrowserVaultFields, parseVaultFieldMappings, vaultFieldMappingProperties, vaultFieldInjectionDescription, type BrowserVaultFieldsResolver } from './browser-vault-injection';
import { PrivateVaultSaves, type PrivateVaultSave } from "./browser-vault-save";
import { createBrowserSession, deleteBrowserSession, type BrowserBinding } from "agents/browser";
import type { NamedTool, ToolContext } from "nanocodex";
import { parseBrowserLoginRequest, browserLoginIdentity } from "./browser-login";
import { PrivateBrowserContinuationSession, snapshotBrowserVault, actBrowserVault, selectBrowserVaultInput, parseBrowserVaultInputSelection, browserVaultInputSelectionProperties, type BrowserVaultIdentity, type BrowserVaultAction } from "./browser-vault";
import { privateVaultTakeover, releasePrivateVaultTakeover, validateBrowserVaultTakeoverAction, rememberPrivateBrowserValues, type BrowserVaultTakeoverAction, type BrowserVaultTouchState } from "./browser-vault-takeover";
import { privateBrowserOperation, parsePrivateBrowserAction } from "./browser-private-operations";

type Login = { id: string; sessionId: string; targetId: string; origin: string; allowedOrigins: string[];
  expiresAt: number; owner: string; phase: "prepared" | "review" | "human" | "finished" };
const TTL = 10 * 60_000;
/** A separate credential browser. No CDP transport or provider URL is exposed to the model.
 * Only metadata is durable; loss of private redaction memory requires a fresh login. */
export function createBrowserLoginRuntime(options: { storage: DurableObjectStorage; browser: BrowserBinding;
  agentId: string; publicOrigin?: string; resolveVaultFields?:BrowserVaultFieldsResolver; savePrivateVault?:PrivateVaultSave; authorize(context: ToolContext): void }) {
  const key = `browser-login:${options.agentId}`, terminalKey = (id:unknown)=>`${key}:terminal:${String(id)}`, owner = crypto.randomUUID();
  const saves = new PrivateVaultSaves(options.savePrivateVault);
  const transport = new PrivateBrowserContinuationSession(options.browser);
  let chain = Promise.resolve(), secrets: string[] = [], segment: {index:number;text:string} | undefined;
  let complete = true, touch: BrowserVaultTouchState = {};
  const exclusive = <T>(run:()=>Promise<T>) => { const result = chain.then(run); chain = result.then(()=>{},()=>{}); return result; };
  const identity = (l: Login): BrowserVaultIdentity => ({vault_id:l.id,target_id:l.targetId,expected_origin:l.origin});
  const metadata = (l: Login) => {
    let loginUrl: string | undefined;
    if (options.publicOrigin) {
      const origin = new URL(options.publicOrigin);
      if (origin.protocol === "https:" && origin.origin === options.publicOrigin) {
        const url = new URL("/browser-login",origin); url.searchParams.set("agent",options.agentId);url.searchParams.set("request",l.id);loginUrl=url.href;
      }
    }
    return {type:"browser_login",status:"input_required",request_id:l.id,challenge_id:l.id,agent_id:options.agentId,
      origin:l.origin,allowed_origins:l.allowedOrigins,expires_at:l.expiresAt,approved:!["prepared","review"].includes(l.phase),...(loginUrl?{login_url:loginUrl}:{})};
  };
  const close = async () => {
    transport.close(); const login = await options.storage.get<Login>(key);
    // Discard admission before cleanup. A failed provider close cannot restore model access.
    await options.storage.delete(key); saves.clear(); secrets=[];segment=undefined;complete=true;touch={};
    if(login)try{await deleteBrowserSession(options.browser,login.sessionId);}catch{/* provider expiry remains the backstop */}
  };
  const current = async (id:unknown, phase?:Login["phase"]) => {
    const login=await options.storage.get<Login>(key);
    if(!login || id!==login.id || login.owner!==owner || login.expiresAt<=Date.now() || !complete || (phase && login.phase!==phase))
      throw new Error("Private login unavailable; close it and request a fresh sign-in");
    return login;
  };
  const remember = (action:Record<string,unknown>) => {
    if (action.action === "fill_fields") {
      segment = undefined;
      if (!rememberPrivateBrowserValues(secrets,
        (action as Extract<BrowserVaultTakeoverAction, {action:"fill_fields"}>).fields.map(field => field.value))) complete = false;
      return;
    }
    if(action.action==="click" || (action.action==="touch" && action.phase==="start") || (action.action==="key" && ["Enter","Tab","Escape"].includes(String(action.key))))segment=undefined;
    const text=["type","edit"].includes(String(action.action)) && typeof action.text==="string" ? action.text : "";
    const deleted=action.action==="edit" ? Number(action.delete_backward) : action.action==="key" && action.key==="Backspace" ? 1:0;
    if(!text && !deleted)return;
    if(!segment)segment={index:secrets.push("")-1,text:""};
    if(deleted && segment.text)secrets.push(segment.text);
    const chars=Array.from(new Intl.Segmenter(undefined,{granularity:"grapheme"}).segment(segment.text),p=>p.segment);
    segment.text=chars.slice(0,Math.max(0,chars.length-deleted)).join("")+text;secrets[segment.index]=segment.text;
    if(secrets.length>128 || secrets.reduce((n,v)=>n+v.length,0)>65536){complete=false;segment=undefined;}
  };
  const requestId = (input:unknown, extra:string[]=[]) => {
    if(!input || typeof input!=="object" || Array.isArray(input) || Object.keys(input).some(k=>!["request_id",...extra].includes(k)))throw new Error("Invalid private login request");
    const v=input as Record<string,unknown>;
    if(typeof v.request_id!=="string" || !/^[0-9a-f-]{36}$/i.test(v.request_id))throw new Error("Invalid private login request");
    return v;
  };
  const tools: NamedTool[] = [{name:"request_browser_login",supportsParallelToolCalls:false,
    description:"Open a retained private browser and ask the user to sign in in the native app or trusted local TUI. The TUI defaults to Save to Vault with a visible opt-out for reusable values; codes stay transient. Only vault_save confirms saving; retry a failed save separately without repeating input. Supply one stable operation_id UUID, a public HTTPS URL, and the exact allowed_origins required for authentication redirects/frames. The user reviews the sites before typing. Prefer defer_input=true to inspect the page before asking for input: this returns page_ready without opening a sheet. Read browser_login_snapshot and choose native_input fields with request_browser_login_input, or omit selection for a browser fallback. The native app or trusted local TUI presents the private input form when requested. Use login_url only when the client cannot render native intake or the user explicitly asks for the browser fallback. Passwords, codes and private screenshots never enter chat or model tools. Wait for browser_login_receipt; finished is not proof of account access. Continue with browser_login_snapshot/action using request_id. Browser authentication does not authenticate a CLI or export cookies. Reuse the same operation ID after uncertainty; never silently retry login.",
    parameters:{type:"object",additionalProperties:false,properties:{operation_id:{type:"string"},url:{type:"string"},allowed_origins:{type:"array",items:{type:"string"},minItems:1,maxItems:8},defer_input:{type:"boolean"}},required:["operation_id","url"]},
    handler:(input,ctx)=>exclusive(async()=>{
      options.authorize(ctx);ctx.signal.throwIfAborted();const request=parseBrowserLoginRequest(input);
      return privateBrowserOperation({storage:options.storage,scope:key,operationId:request.operationId,input:request,run:async()=>{
        if(await options.storage.get(key))throw new Error("Close the existing private login first");
        // Tombstone precedes allocation: interrupted/uncertain allocation cannot be retried under another ID.
        await options.storage.put(key,{id:request.operationId,owner,phase:"review",expiresAt:Date.now()+TTL});
        let sessionId:string|undefined;
        try{
          const opened=await createBrowserSession(options.browser,{keepAliveMs:TTL,recording:false});sessionId=opened.sessionId;
          const provisional={id:request.operationId,sessionId,targetId:"pending",origin:new URL(request.url).origin,allowedOrigins:request.allowedOrigins,expiresAt:Date.now()+TTL,owner,phase:request.deferInput ? "prepared" as const : "review" as const};
          await options.storage.put(key,provisional);
          return await transport.run(sessionId,identity(provisional),ctx.signal,async cdp=>{
            const {targetId}=await cdp.send("Target.createTarget",{url:"about:blank"});
            if(typeof targetId!=="string" || !/^[A-Za-z0-9_-]{1,128}$/.test(targetId))throw new Error();
            const login={...provisional,targetId};await options.storage.put(key,login);
            const attached=await cdp.attachTarget(targetId);
            const result=await cdp.send("Page.navigate",{url:request.url},attached.sessionId);if(result.errorText)throw new Error();
            return request.deferInput ? {type:"browser_login",status:"page_ready",request_id:login.id,next_action:"read_snapshot_and_request_input"} : metadata(login);
          });
        }catch{transport.close();if(sessionId)try{await deleteBrowserSession(options.browser,sessionId);}catch{}throw new Error("Private login opening could not be confirmed");}
      }});
    })},
    {name:"request_browser_login_input",supportsParallelToolCalls:false,
      description:"Ask for more private input in the same retained login browser, such as a later password or verification code. First read browser_login_snapshot, then supply snapshot_id and fields [{ref,label?}] to choose the native inputs from that page, optionally with a short reason explaining what the user should enter. All input types supported by native_input=true are eligible, including ordinary text, multiline notes, selects and checkboxes. Labels describe the existing fields; never supply input values or secrets. Browser-derived keyboard/autofill hints are preserved. A stale_page result leaves the current request intact: read a fresh snapshot and use a new operation_id. Requires the current request_id and one stable operation_id UUID. The native app automatically presents the secure input sheet; use login_url only when native intake is unavailable or the user requests the browser fallback. Returns a fresh request_id/challenge_id without navigating, creating a browser, or losing login state. Use the new request_id for subsequent snapshots/actions. Wait for browser_login_receipt, then inspect a snapshot. Reuse identical arguments after uncertainty; never repeat the browser action that prompted the input.",
      parameters:{type:"object",additionalProperties:false,properties:{request_id:{type:"string"},operation_id:{type:"string"},...browserVaultInputSelectionProperties},required:["request_id","operation_id"]},
      handler:(input,ctx)=>exclusive(async()=>{
        options.authorize(ctx);ctx.signal.throwIfAborted();const v=requestId(input,["operation_id","snapshot_id","fields","reason"]);
        const selection = parseBrowserVaultInputSelection(v,secrets);
        // Look up the operation before the current request: a successful request
        // rotates the identity, but an identical retry must return its same panel.
        return privateBrowserOperation({storage:options.storage,scope:key,operationId:v.operation_id,input:{action:"request_input",request_id:v.request_id,...(selection ? {snapshot_id:v.snapshot_id,selection} : {})},run:async()=>{
          const login=await current(v.request_id);
          if (!["prepared","finished"].includes(login.phase)) throw new Error("Private login is under user control");
          return transport.run(login.sessionId,identity(login),ctx.signal,async cdp=>{
            const bound=await browserLoginIdentity(cdp,identity(login),login.allowedOrigins);
            const selected = selection ? await selectBrowserVaultInput(cdp,bound,v.snapshot_id as string,selection) : undefined;
            if (selection && !selected) return {status:"stale_page",request_id:login.id,next_action:"read_snapshot_and_request_input"};
            const next:Login={...login,id:crypto.randomUUID(),origin:bound.expected_origin,phase:login.phase === "prepared" ? "review" : "human",expiresAt:Date.now()+TTL};
            await options.storage.put(key,next);
            touch=selected ? {nativeSelection:selected} : {};segment=undefined;
            return metadata(next);
          });
        }});
      })},
    {name:"browser_login_snapshot",supportsParallelToolCalls:false,
      description:"Read a bounded redacted page view after defer_input=true preparation or after the user finishes private input. Native-input eligibility and types let you choose a grounded native sheet with request_browser_login_input. Requires request_id. Finished is not proof of authentication; verify account content. No input values, cookies, raw DOM, screenshots or provider URLs. Unavailable during human control or after runtime recovery; close and request new login if private state is lost.",
      parameters:{type:"object",additionalProperties:false,properties:{request_id:{type:"string"}},required:["request_id"]},
      handler:(input,ctx)=>exclusive(async()=>{options.authorize(ctx);const v=requestId(input),login=await current(v.request_id);
        if (!["prepared","finished"].includes(login.phase)) throw new Error("Private login is under user control");
        return transport.run(login.sessionId,identity(login),ctx.signal,async cdp=>{
          const bound=await browserLoginIdentity(cdp,identity(login),login.allowedOrigins);
          return snapshotBrowserVault(cdp,bound,secrets);
        });})},
    {name:"browser_login_action",supportsParallelToolCalls:false,
      description:"Continue a prepared page before requesting input, or a one-time private browser after user handback, using request_id and a stable operation_id. Use refs from browser_login_snapshot. Only perform actions authorized by the user. Passwords and verification codes must be entered through the private native app or trusted TUI form, never text arguments. Navigation remains on the current approved origin. Inspect a new snapshot after every action; action_requested is not confirmation. No CLI credential export.",
      parameters:{type:"object",additionalProperties:false,properties:{request_id:{type:"string"},operation_id:{type:"string"},action:{type:"string",enum:["navigate","click","fill","select","check"]},url:{type:"string"},snapshot_id:{type:"string"},ref:{type:"string"},text:{type:"string"},option_index:{type:"integer"},checked:{type:"boolean"}},required:["request_id","operation_id","action"]},
      handler:(input,ctx)=>exclusive(async()=>{options.authorize(ctx);const v=requestId(input,["operation_id","action","url","snapshot_id","ref","text","option_index","checked"]);
        const login=await current(v.request_id),action=parsePrivateBrowserAction(v) as BrowserVaultAction;
        if (!["prepared","finished"].includes(login.phase)) throw new Error("Private login is under user control");
        return privateBrowserOperation({storage:options.storage,scope:key,operationId:v.operation_id,input:{request_id:v.request_id,action},run:()=>transport.run(login.sessionId,identity(login),ctx.signal,async cdp=>{
          const bound=await browserLoginIdentity(cdp,identity(login),login.allowedOrigins);return actBrowserVault(cdp,bound,action);
        })});})},
    ...(options.resolveVaultFields ? [{name:'browser_login_inject_fields',supportsParallelToolCalls:false,
      description:vaultFieldInjectionDescription,
      parameters:{type:'object',additionalProperties:false,properties:{request_id:{type:'string'},...vaultFieldMappingProperties},required:['request_id','snapshot_id','operation_id','fields']},
      handler:(input:unknown,ctx:ToolContext)=>exclusive(async()=>{
        options.authorize(ctx);const v=requestId(input,['operation_id','snapshot_id','fields']),login=await current(v.request_id);
        if(!['prepared','finished'].includes(login.phase))throw new Error('Private login is under user control');
        const mappings=parseVaultFieldMappings(v.fields);
        if(typeof v.snapshot_id !== 'string')throw new Error('Invalid private snapshot');
        return privateBrowserOperation({storage:options.storage,scope:key,operationId:v.operation_id,
          input:{kind:'vault_fields',request_id:v.request_id,snapshot_id:v.snapshot_id,mappings},
          run:()=>transport.run(login.sessionId,identity(login),ctx.signal,async cdp=>{
            const bound=await browserLoginIdentity(cdp,identity(login),login.allowedOrigins);
            return injectBrowserVaultFields({cdp,identity:bound,snapshotId:v.snapshot_id as string,mappings,resolve:options.resolveVaultFields!,
              remember:values=>{if(!rememberPrivateBrowserValues(secrets,values)){complete=false;throw new Error('Private input limit reached');}},context:ctx});
          })});
      })}] : []),
    {name:"browser_login_close",supportsParallelToolCalls:false,description:"Discard a one-time private login session and its credentials. Closes even after expiry or recovery; does not repeat login or account actions.",
      parameters:{type:"object",additionalProperties:false,properties:{},required:[]},handler:(input,ctx)=>exclusive(async()=>{options.authorize(ctx);if(!input || typeof input!=="object" || Object.keys(input).length)throw new Error("Invalid close");await close();return {status:"closed"};})}
  ];
  const submit = (input:unknown,signal:AbortSignal) => exclusive(async()=>{
    if(!input || typeof input!=="object" || Array.isArray(input))throw new Error("Invalid private login control");
    const v=input as Record<string,unknown>;
    if(v.action==="retry_vault_save" && Object.keys(v).length===2 && typeof v.challenge_id==="string")return {vault_save:await saves.retry(v.challenge_id)};
    const stored=await options.storage.get<Login>(key);
    if(!stored || stored.id!==v.challenge_id){
      if(["cancel","finish"].includes(String(v.action)) && Object.keys(v).length===2){
        const prior=await options.storage.get<{status:string}>(terminalKey(v.challenge_id));
        if(prior?.status===(v.action==="finish"?"finished":"cancelled"))return prior;
      }
      throw new Error("Private login unavailable");
    }
    signal.throwIfAborted();const {challenge_id,...action}=v;
    if(["cancel","describe","approve","finish"].includes(String(action.action))){
      if(Object.keys(action).length!==1)throw new Error("Invalid private login control");
      if(action.action==="cancel"){const receipt={type:"browser_login_receipt",status:"cancelled",request_id:stored.id};await options.storage.put(terminalKey(stored.id),receipt);await close();return receipt;}
      if(action.action==="finish" && stored.phase==="finished"){const vault_save=await saves.finish(stored.id);return {type:"browser_login_receipt",status:"finished",request_id:stored.id,...(vault_save?{vault_save}:{})};}
      const login=await current(v.challenge_id);
      if(action.action==="describe")return metadata(login);
      if(action.action==="approve"){
        if(login.phase!=="review" && login.phase!=="human")throw new Error("Private login is finished");
        await options.storage.put(key,{...login,phase:"human"});return {status:"approved"};
      }
      if(login.phase!=="human")throw new Error("Private login is not active");
      // Finish relinquishes user control; account access still requires a model snapshot check.
      await transport.run(login.sessionId,identity(login),signal,cdp=>releasePrivateVaultTakeover(cdp,login.targetId));
      const vault_save=await saves.finish(login.id);
      const receipt={type:"browser_login_receipt",status:"finished",request_id:login.id,...(vault_save?{vault_save}:{})};
      await options.storage.transaction(async tx=>{
        await tx.put(key,{...login,phase:"finished",expiresAt:Date.now()+TTL});
        await tx.put(terminalKey(login.id),receipt);
      });touch={};segment=undefined;
      return receipt;
    }
    const login=await current(v.challenge_id,"human");
    if(action.action === 'fill_vault_fields') {
      if(!options.resolveVaultFields)throw new Error('Vault reuse unavailable');
      const selected=parseNativeVaultInjection(action);
      try { return await transport.run(login.sessionId,identity(login),signal,async cdp=>{
        const bound=await browserLoginIdentity(cdp,identity(login),login.allowedOrigins);
        return injectNativeVaultFields({cdp,identity:bound,touch,...selected,resolve:options.resolveVaultFields!,signal,
          remember:values=>{segment=undefined;if(!rememberPrivateBrowserValues(secrets,values)){complete=false;throw new Error('Private input limit reached');}}});
      }); } catch { throw new Error('Private Vault input could not be confirmed; refresh before continuing'); }
    }
    validateBrowserVaultTakeoverAction(action as BrowserVaultTakeoverAction);remember(action);
    try{return await transport.run(login.sessionId,identity(login),signal,async cdp=>{
      // privateVaultTakeover validates every frame against allowedOrigins before and
      // after input and capture; the current top-level origin is reported in touch.
      delete touch.origin;
      const frame=await privateVaultTakeover(cdp,identity(login),action as BrowserVaultTakeoverAction,touch,false,login.allowedOrigins,(origin,form,values,enabled,details)=>saves.stage(login.id,origin,form,values,enabled,login.sessionId+":"+login.targetId,details));
      if(!touch.origin || !login.allowedOrigins.includes(touch.origin))throw new Error("Private login reached an unapproved site");
      return {...frame,origin:touch.origin};
    });}catch{saves.cancelScope(login.sessionId+":"+login.targetId,login.origin);touch.uncertain=true;throw new Error("Private browser action could not be confirmed; refresh before continuing");}
  });
  return {tools,submit,owns:async(id:unknown)=>{const l=await options.storage.get<Login>(key);return (!!l&&l.id===id)||!!await options.storage.get(terminalKey(id));},
    close:()=>exclusive(close),expire:()=>exclusive(async()=>{saves.expire();const l=await options.storage.get<Login>(key);if(l && l.expiresAt<=Date.now())await close();})};
}
