import { validatePrivateVaultSave, validatePrivateVaultDetails, type PrivateVaultDetails } from "./browser-vault-save";
import { NATIVE_FORM_STATE, PrivateBrowserNoActiveTouch, isBrowserVaultOrigin, type BrowserVaultIdentity, type PrivateBrowserCdp } from "./browser-vault";

export type BrowserVaultTakeoverAction =
  | { action: "observe"; image_format?: "jpeg"; native_fields?: boolean; native_field_hints?: boolean; native_field_controls?: boolean; viewport?: { width: number; height: number; mobile: boolean } }
  | { action: "click"; x: number; y: number }
  | { action: "type"; text: string }
  | { action: "fill_fields"; document_id: string; fields: { ref: string; value: string }[]; save_to_vault?: boolean; save_details?: PrivateVaultDetails }
  | { action: "edit"; delete_backward: number; text: string }
  | { action: "touch"; phase: "start" | "move" | "end" | "cancel"; x?: number; y?: number }
  | { action: "key"; key: "Enter" | "Tab" | "Backspace" | "Escape" }
  | { action: "scroll"; delta_y: number };
export type BrowserVaultTouchState = { jpeg?: boolean; origin?: string; active?: boolean; uncertain?: boolean; nativeFields?: boolean; nativeFieldHints?: boolean; nativeFieldControls?: boolean; nativeSelection?: string; nativeForm?: { documentId: string; contextId: number; frameId: string; loaderId: string; origin: string; form: BrowserVaultNativeForm } };
export type BrowserVaultKeyboard = { type: "text" | "email" | "url" | "tel" | "number" | "password"; multiline: boolean };
const NATIVE_AUTOCOMPLETE = ["username", "current-password", "new-password", "one-time-code", "email", "tel", "cc-number", "cc-exp", "cc-exp-month", "cc-exp-year", "cc-csc", "name", "given-name", "family-name", "street-address", "postal-code", "address-line1", "address-line2", "address-level1", "address-level2", "country", "country-name"] as const;
const NATIVE_INPUTMODES = ["text", "email", "url", "tel", "numeric", "decimal", "search"] as const;
export type BrowserVaultNativeForm = { document_id: string; reason?: string; fields: {type: BrowserVaultKeyboard["type"] | "select" | "checkbox"; multiline: boolean; ref: string; label: string; options?: {index:number;label:string}[]; checked?: boolean; autocomplete?: typeof NATIVE_AUTOCOMPLETE[number]; inputmode?: typeof NATIVE_INPUTMODES[number] }[] };
export type BrowserVaultTakeoverResult = { native_form?: BrowserVaultNativeForm; native_form_status?: "stale"; status: "active"; image: string; width: number; height: number; keyboard?: BrowserVaultKeyboard; inputs?: (BrowserVaultKeyboard & { x: number; y: number; width: number; height: number })[] };

/** Register before dispatch: browser value sanitization may finish even if CDP
 * loses its reply. Keep raw and all supported native-input normalization variants
 * without depending on post-fill DOM inspection or exposing values in metadata. */
export function rememberPrivateBrowserValues(secrets: string[], values: readonly string[]): boolean {
  const trimAscii = (value: string) => value.replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, "");
  for (const value of values) {
    const singleLine = value.replace(/[\r\n]/g, "");
    const variants = [value, singleLine, trimAscii(singleLine),
      singleLine.split(",").map(trimAscii).join(","), value.replace(/\r\n?/g, "\n")];
    for (const variant of variants) if (variant && !secrets.includes(variant)) secrets.push(variant);
  }
  return secrets.length <= 128 && secrets.reduce((size, value) => size + value.length, 0) <= 65536;
}

const MAX_IMAGE_BASE64 = 8 * 1024 * 1024;
const keys = { Enter: 13, Tab: 9, Backspace: 8, Escape: 27 } as const;
export function validateBrowserVaultTakeoverAction(value: BrowserVaultTakeoverAction) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
  let allowed: string[];
  switch (value.action) {
    case "observe":
      if (value.image_format !== undefined && value.image_format !== "jpeg") throw new Error();
      if (value.native_fields !== undefined && typeof value.native_fields !== "boolean") throw new Error();
      if (value.native_field_hints !== undefined && (typeof value.native_field_hints !== "boolean" || value.native_fields !== true)) throw new Error();
      if (value.native_field_controls !== undefined && (typeof value.native_field_controls !== "boolean" || value.native_fields !== true)) throw new Error();
      if (value.viewport !== undefined) {
        const v = value.viewport;
        if (!v || typeof v !== "object" || Array.isArray(v) || typeof v.mobile !== "boolean"
          || ![v.width,v.height].every(n => Number.isInteger(n) && n >= 240 && n <= 1920)
          || Object.keys(v).some(k => !["width","height","mobile"].includes(k))) throw new Error();
      }
      allowed = ["action", "image_format", "viewport", "native_fields", "native_field_hints", "native_field_controls"]; break;
    case "click":
      if (![value.x, value.y].every(n => Number.isFinite(n) && n >= 0 && n <= 1)) throw new Error();
      allowed = ["action", "x", "y"]; break;
    case "fill_fields": {
      if (typeof value.document_id !== "string" || !/^[0-9a-f-]{36}$/.test(value.document_id)
        || !Array.isArray(value.fields) || !value.fields.length || value.fields.length > 32) throw new Error();
      const refs = new Set<string>(); let size = 0;
      for (const field of value.fields) {
        if (!field || typeof field !== "object" || Array.isArray(field)
          || Object.keys(field).some(k => !["ref", "value"].includes(k))
          || typeof field.ref !== "string" || !/^[0-9a-f-]{36}$/.test(field.ref) || refs.has(field.ref)
          || typeof field.value !== "string" || field.value.length > 4096 || field.value.includes("\0")) throw new Error();
        refs.add(field.ref); size += new TextEncoder().encode(field.value).length;
      }
      if (size > 32768) throw new Error();
      if(value.save_to_vault !== undefined)validatePrivateVaultSave(value.save_to_vault);
      if(value.save_details !== undefined){if(value.save_to_vault !== true)throw new Error();validatePrivateVaultDetails(value.save_details);}
      allowed = ["action", "document_id", "fields", "save_to_vault", "save_details"]; break;
    }
    case "type":
      if (typeof value.text !== "string" || !value.text.length || value.text.length > 512) throw new Error();
      allowed = ["action", "text"]; break;
    case "edit":
      if (!Number.isInteger(value.delete_backward) || value.delete_backward < 0 || value.delete_backward > 128
        || typeof value.text !== "string" || value.text.length > 512) throw new Error();
      allowed = ["action", "delete_backward", "text"]; break;
    case "touch":
      if (!["start", "move", "end", "cancel"].includes(value.phase)) throw new Error();
      if (value.phase === "start" || value.phase === "move" || value.x !== undefined || value.y !== undefined) {
        if (![value.x, value.y].every(n => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= 1)) throw new Error();
      }
      allowed = ["action", "phase", "x", "y"]; break;
    case "key":
      if (!Object.hasOwn(keys, value.key)) throw new Error();
      allowed = ["action", "key"]; break;
    case "scroll":
      if (!Number.isFinite(value.delta_y) || Math.abs(value.delta_y) > 2000) throw new Error();
      allowed = ["action", "delta_y"]; break;
    default: throw new Error();
  }
  if (Object.keys(value).some(key => !allowed.includes(key))) throw new Error();
}

/** HUMAN HTTP RESPONSE ONLY. Never register as a model tool or log its input/output.
 * Caller authenticates the human, holds the exclusive bounded takeover lease, blocks
 * all model access, and caches the private connection across gesture operations. The
 * caller owns connection cleanup; the quarantined browser and lease remain for
 * explicit refresh/recovery. Origin checks
 * bracket each input and screenshot; they cannot make browser navigation atomic.
 */
export async function privateVaultTakeover(
  cdp: Pick<PrivateBrowserCdp, "send"> & Partial<Pick<PrivateBrowserCdp, "attachTarget">>, identity: BrowserVaultIdentity, action: BrowserVaultTakeoverAction,
  touch: BrowserVaultTouchState = {},
  restoreViewport = false,
  allowedOrigins?: readonly string[],
  onFilled?: (origin:string, form:BrowserVaultNativeForm, values:{ref:string;value:string}[], enabled:boolean, details?:PrivateVaultDetails)=>void,
): Promise<BrowserVaultTakeoverResult> {
  let sid: string | undefined;
  // Value-free performance trace: action kind, wall time, CDP calls, image size.
  const started = Date.now(), base = cdp; let cdpCalls = 0, imageBytes = 0, measured: string | undefined;
  cdp = { send: (method: string, params?: unknown, session?: string) => { cdpCalls++; return base.send(method, params, session); },
    ...(base.attachTarget ? { attachTarget: (id: string) => base.attachTarget!(id) } : {}) } as typeof cdp;
  try {
    validateBrowserVaultTakeoverAction(action);
    measured = action.action;
    if (action.action === "touch") {
      if (action.phase !== "cancel" && (touch.uncertain || (action.phase === "start" ? touch.active : !touch.active))) throw new Error();
    } else if (action.action !== "observe" && (touch.active || touch.uncertain)) throw new Error();
    if (!identity || !isBrowserVaultOrigin(identity.expected_origin)
      || typeof identity.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(identity.vault_id)
      || typeof identity.target_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(identity.target_id)) throw new Error();
    const sameOrigin = (value: unknown) => {
      if (typeof value !== "string") throw new Error();
      const url = new URL(value);
      if (url.protocol !== "https:" || !(allowedOrigins ?? [identity.expected_origin]).includes(url.origin) || url.username || url.password) throw new Error();
    };
    const attached = cdp.attachTarget ? await cdp.attachTarget(identity.target_id) : await cdp.send("Target.attachToTarget", { targetId: identity.target_id, flatten: true });
    if (typeof attached?.sessionId !== "string" || !attached.sessionId) throw new Error();
    sid = attached.sessionId;
    let frameId = "", loaderId = "", currentOrigin = "";
    // One pipelined round trip: target and frame tree are requested together.
    // Login takeovers validate every frame against the approved origins.
    const check = async () => {
      const [info, tree] = await Promise.all([
        cdp.send("Target.getTargetInfo", { targetId: identity.target_id }),
        cdp.send("Page.getFrameTree", {}, sid)]);
      const targetInfo = info?.targetInfo;
      if (targetInfo?.type !== "page" || (targetInfo.targetId !== undefined && targetInfo.targetId !== identity.target_id)) throw new Error();
      sameOrigin(targetInfo.url);
      const frame = tree?.frameTree?.frame;
      if (!frame || frame.parentId || typeof frame.id !== "string" || !frame.id) throw new Error();
      sameOrigin(frame.url);
      const origin = new URL(frame.url).origin;
      if (allowedOrigins) {
        if (new URL(targetInfo.url).origin !== origin) throw new Error();
        let count = 0;
        const visit = (node: any, parent: string) => {
          if (++count > 128 || !node?.frame) throw new Error();
          const url = node.frame.url;
          let current = parent;
          if (!(url === "about:blank" || url === "about:srcdoc")) { sameOrigin(url); current = new URL(url).origin; }
          for (const child of node.childFrames ?? []) visit(child, current);
        };
        for (const child of tree.frameTree.childFrames ?? []) visit(child, origin);
      }
      frameId = frame.id; loaderId = typeof frame.loaderId === "string" ? frame.loaderId : ""; currentOrigin = origin;
    };
    // Non-observe actions never change emulation, so layout metrics share the
    // initial check's round trip. Observation may resize and measures afterwards.
    const early = action.action === "observe" ? undefined : cdp.send("Page.getLayoutMetrics", {}, sid);
    early?.catch(() => {});
    await check();
    if (action.action === "observe") {
      touch.nativeFields = action.native_fields === true;
      touch.nativeFieldHints = touch.nativeFields && action.native_field_hints === true;
      touch.nativeFieldControls = touch.nativeFields && action.native_field_controls === true;
      touch.jpeg = action.image_format === "jpeg";
      // Observation is explicit recovery after an ambiguous gesture, never a replay.
      // Chrome rejects touchCancel when no touch sequence has started.
      if (touch.active || touch.uncertain) {
        try { await cdp.send("Input.dispatchTouchEvent", { type: "touchCancel", touchPoints: [] }, sid); }
        catch (error) {
          // A replaced channel can have no finger despite uncertain lease state.
          // Only Chrome's specific absent-sequence rejection confirms recovery.
          if (!(error instanceof PrivateBrowserNoActiveTouch)) throw error;
        }
      }
      touch.active = false; touch.uncertain = false;
      if (restoreViewport) {
        await Promise.all([cdp.send("Emulation.clearDeviceMetricsOverride", {}, sid),
          cdp.send("Emulation.setTouchEmulationEnabled", { enabled: false }, sid)]);
        await check();
      } else if (action.viewport) {
        await Promise.all([cdp.send("Emulation.setDeviceMetricsOverride", { ...action.viewport, deviceScaleFactor: 1 }, sid),
          cdp.send("Emulation.setTouchEmulationEnabled", { enabled: action.viewport.mobile, maxTouchPoints: 1 }, sid)]);
        await check();
      }
    }
    const metrics = await (early ?? cdp.send("Page.getLayoutMetrics", {}, sid));
    const viewport = metrics?.cssLayoutViewport;
    const width = viewport?.clientWidth, height = viewport?.clientHeight;
    if (![width, height].every(n => typeof n === "number" && Number.isInteger(n) && n > 0 && n <= 8192)
      || width * height > 16_777_216) throw new Error();
    // Every input is bracketed by origin checks; a leading check is skipped
    // only when the immediately preceding operation already ended with one.
    let checked = true;
    const input = async (method: string, params: unknown) => {
      if (!checked) await check();
      checked = false;
      await cdp.send(method, params, sid);
      await check(); checked = true;
    };
    if (action.action === "fill_fields") {
      const binding = touch.nativeForm;
      delete touch.nativeForm; // A batch is single-use even if its response is lost.
      if (!binding || binding.documentId !== action.document_id || binding.frameId !== frameId
        || !loaderId || binding.loaderId !== loaderId || binding.origin !== currentOrigin) throw new Error();
      touch.uncertain = true;
      await check();
      const filled = await cdp.send("Runtime.callFunctionOn", {
        executionContextId: binding.contextId, returnByValue: true, silent: true,
        functionDeclaration: NATIVE_FORM_FILL,
        arguments: [binding.documentId, binding.origin, action.fields, action.save_to_vault === true].map(value => ({value})),
      }, sid);
      await check();
      const result = filled?.result?.value;
      if (filled?.exceptionDetails || (action.save_to_vault === true ? result?.filled !== true || !Array.isArray(result.fields) : result !== true)) throw new Error();
      touch.uncertain = false;
      if(action.save_to_vault !== undefined)onFilled?.(currentOrigin,binding.form,action.save_to_vault ? result.fields : action.fields,action.save_to_vault,action.save_details);
    } else if (action.action === "touch") {
      // Mark uncertain before sending: a disconnected response must never replay input.
      touch.uncertain = true;
      await input("Input.dispatchTouchEvent", {
        type: { start: "touchStart", move: "touchMove", end: "touchEnd", cancel: "touchCancel" }[action.phase],
        touchPoints: action.phase === "start" || action.phase === "move"
          ? [{ x: Math.min(action.x! * width, width - 1), y: Math.min(action.y! * height, height - 1), id: 0 }] : [],
      });
      touch.active = action.phase === "start" || action.phase === "move";
      touch.uncertain = false;
    } else if (action.action === "edit") {
      // One bracketed, pipelined batch: CDP applies messages in order on one socket.
      const events: Promise<unknown>[] = [];
      for (let i = 0; i < action.delete_backward; i++) {
        events.push(cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 }, sid));
        events.push(cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 }, sid));
      }
      if (action.text) events.push(cdp.send("Input.insertText", { text: action.text }, sid));
      if (events.length) { checked = false; await Promise.all(events); await check(); checked = true; }
    } else if (action.action === "click") {
      const position = { x: Math.min(action.x * width, width - 1), y: Math.min(action.y * height, height - 1), button: "left", clickCount: 1 };
      await input("Input.dispatchMouseEvent", { type: "mousePressed", ...position });
      await input("Input.dispatchMouseEvent", { type: "mouseReleased", ...position });
    } else if (action.action === "type") {
      await input("Input.insertText", { text: action.text });
    } else if (action.action === "key") {
      const key = { key: action.key, code: action.key, windowsVirtualKeyCode: keys[action.key] };
      await input("Input.dispatchKeyEvent", { type: "keyDown", ...key });
      await input("Input.dispatchKeyEvent", { type: "keyUp", ...key });
    } else if (action.action === "scroll") {
      await input("Input.dispatchMouseEvent", { type: "mouseWheel", x: width / 2, y: height / 2, deltaX: 0, deltaY: action.delta_y });
    }
    if (!checked) await check();
    // Fixed isolated-world code returns only an allowlisted descriptor, never field values.
    let keyboard: BrowserVaultKeyboard | undefined;
    let inputs: BrowserVaultTakeoverResult["inputs"];
    let nativeForm: BrowserVaultNativeForm | undefined;
    delete touch.nativeForm;
    // Metadata discovery and capture are pipelined; the final origin check
    // below still gates every returned byte.
    const capture = cdp.send("Page.captureScreenshot", touch.jpeg
      ? { format: "jpeg", quality: 70, fromSurface: true, captureBeyondViewport: false, optimizeForSpeed: true }
      : { format: "png", fromSurface: true, captureBeyondViewport: false }, sid);
    capture.catch(() => {});
    const keyboardTask = (async () => { try {
      const world = await cdp.send("Page.createIsolatedWorld", { frameId, worldName: "nanocodex-private-keyboard", grantUniveralAccess: false }, sid);
      if (Number.isInteger(world?.executionContextId)) {
        const result = await cdp.send("Runtime.callFunctionOn", {
          executionContextId: world.executionContextId, returnByValue: true,
          functionDeclaration: `function() {
            const describe = e => {
              if (!e || e.disabled || e.readOnly) return null;
              if (e.tagName === "TEXTAREA" || e.isContentEditable) return {type:"text",multiline:true};
              if (e.tagName !== "INPUT") return null;
              const t = e.type;
              if (!["text","search","email","url","tel","number","password"].includes(t)) return null;
              return {type:t === "search" ? "text" : t,multiline:false};
            };
            const inputs = [];
            for (const field of document.querySelectorAll('input,textarea,[contenteditable]')) {
              if (inputs.length >= 32) break;
              const descriptor = describe(field), r = field.getBoundingClientRect();
              if (!descriptor || !r.width || !r.height || !field.checkVisibility({checkOpacity:true,checkVisibilityCSS:true}) || field.closest('[inert],[hidden],[aria-hidden="true"]')) continue;
              const x = Math.max(0,r.left), y = Math.max(0,r.top), right = Math.min(innerWidth,r.right), bottom = Math.min(innerHeight,r.bottom);
              if (right <= x || bottom <= y) continue;
              inputs.push({...descriptor,x:x/innerWidth,y:y/innerHeight,width:(right-x)/innerWidth,height:(bottom-y)/innerHeight});
            }
            let e = document.activeElement;
            for (let i = 0; i < 8 && e; i++) {
              if (e.tagName === "IFRAME") { try { e = e.contentDocument?.activeElement; } catch { e = null; } }
              else if (e.shadowRoot?.activeElement) e = e.shadowRoot.activeElement;
              else break;
            }
            return {keyboard:describe(e),inputs};
          }`,
        }, sid);
        const metadata = result?.result?.value;
        const v = metadata?.keyboard;
        if (!result?.exceptionDetails && v && ["text", "email", "url", "tel", "number", "password"].includes(v.type) && typeof v.multiline === "boolean")
          keyboard = { type: v.type, multiline: v.multiline };
        if (!result?.exceptionDetails && Array.isArray(metadata?.inputs) && metadata.inputs.length <= 32) {
          inputs = metadata.inputs.filter((r: any) => r && ["text", "email", "url", "tel", "number", "password"].includes(r.type)
            && typeof r.multiline === "boolean" && [r.x,r.y,r.width,r.height].every(n => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= 1)
            && r.width > 0 && r.height > 0 && r.x+r.width <= 1.000001 && r.y+r.height <= 1.000001)
            .map((r: any) => ({ type:r.type, multiline:r.multiline, x:r.x, y:r.y, width:r.width, height:r.height }));
        }
      }
    } catch { /* Optional focus metadata is unavailable; never forward provider errors. */ } })();
    // Only opted-in clients receive new metadata; older clients reject unknown keys.
    // Separate optional discovery keeps the viewport usable for custom controls and iframes.
    const nativeTask = (async () => { if (touch.nativeFields) try {
      const world = await cdp.send("Page.createIsolatedWorld", { frameId, worldName: touch.nativeSelection ? "nanocodex-vault-continuation" : "nanocodex-private-native-form", grantUniveralAccess: false }, sid);
      if (Number.isInteger(world?.executionContextId) && loaderId) {
        const documentId = crypto.randomUUID();
        const refs = Array.from({length:32}, () => crypto.randomUUID());
        const result = await cdp.send("Runtime.callFunctionOn", {
          executionContextId: world.executionContextId, returnByValue: true, silent: true,
          functionDeclaration: NATIVE_FORM_DISCOVER,
          arguments: [documentId, currentOrigin, refs, true, touch.nativeFieldControls === true, touch.nativeSelection ?? null].map(value => ({value})),
        }, sid);
        const discovered = result?.result?.value;
        const fields = discovered?.fields;
        if (!result?.exceptionDetails && Array.isArray(fields) && fields.length > 0 && fields.length <= 32
          && fields.every((f: any, i: number) => f && f.ref === refs[i] && typeof f.label === "string" && f.label.length <= 160
            && ["text","email","url","tel","number","password",...(touch.nativeFieldControls ? ["select","checkbox"] : [])].includes(f.type) && typeof f.multiline === "boolean"
            && (f.autocomplete === undefined || NATIVE_AUTOCOMPLETE.includes(f.autocomplete))
            && (f.inputmode === undefined || NATIVE_INPUTMODES.includes(f.inputmode))
            && (f.type !== "checkbox" || typeof f.checked === "boolean")
            && (f.type !== "select" || Array.isArray(f.options) && f.options.length <= 200 && f.options.every((o:any,i:number) =>
              Number.isInteger(o.index) && o.index >= 0 && o.index < 200 && (i === 0 || o.index > f.options[i-1].index) && typeof o.label === "string" && o.label.length <= 160)))) {
          nativeForm = { document_id: documentId, ...(touch.nativeFieldControls && typeof discovered.reason === "string" ? {reason:discovered.reason.slice(0,500)} : {}), fields: fields.map((f: any) => ({ref:f.ref,label:f.label,type:f.type,multiline:f.multiline,
            ...(touch.nativeFieldHints && f.autocomplete ? {autocomplete:f.autocomplete} : {}),
            ...(touch.nativeFieldHints && f.inputmode ? {inputmode:f.inputmode} : {}),
            ...(f.type === "select" ? {options:f.options} : {}), ...(f.type === "checkbox" ? {checked:f.checked} : {})})) };
          touch.nativeForm = {documentId,contextId:world.executionContextId,frameId,loaderId,origin:currentOrigin,form:{...nativeForm,fields:result.result.value.fields}};
        }
      }
    } catch { /* Never forward provider errors. */ } })();
    await Promise.all([keyboardTask, nativeTask]);
    // A confirmed fill may legitimately rerender the form. Its success receipt
    // must not be mistaken for a stale request that still needs user input.
    const nativeFormStale = action.action !== "fill_fields" && touch.nativeFields === true && !!touch.nativeSelection && !nativeForm;
    const screenshot = await capture;
    await check();
    touch.origin = currentOrigin;
    const data = screenshot?.data;
    imageBytes = typeof data === "string" ? data.length : 0;
    if (typeof data !== "string" || data.length < 44 || data.length > MAX_IMAGE_BASE64
      || data.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)) throw new Error();
    if (touch.jpeg) {
      // JPEG pixels match the emulated CSS viewport at deviceScaleFactor 1.
      if (!data.startsWith("/9j/")) throw new Error();
      return { status: "active", image: `data:image/jpeg;base64,${data}`, width, height, ...(keyboard ? { keyboard } : {}), ...(inputs ? { inputs } : {}), ...(nativeForm ? {native_form:nativeForm} : {}), ...(touch.nativeFieldControls && nativeFormStale ? {native_form_status:"stale" as const} : {}) };
    }
    const header = atob(data.slice(0, 44));
    if (header.slice(0, 8) !== "\x89PNG\r\n\x1a\n" || header.slice(12, 16) !== "IHDR") throw new Error();
    const dimension = (offset: number) => [...header.slice(offset, offset + 4)].reduce((n, c) => n * 256 + c.charCodeAt(0), 0);
    const imageWidth = dimension(16), imageHeight = dimension(20);
    if (!imageWidth || !imageHeight || imageWidth > 8192 || imageHeight > 8192 || imageWidth * imageHeight > 16_777_216) throw new Error();
    return { status: "active", image: `data:image/png;base64,${data}`, width: imageWidth, height: imageHeight, ...(keyboard ? { keyboard } : {}), ...(inputs ? { inputs } : {}), ...(nativeForm ? {native_form:nativeForm} : {}), ...(touch.nativeFieldControls && nativeFormStale ? {native_form_status:"stale" as const} : {}) };
  } catch { delete touch.nativeForm; throw new Error("Private browser takeover could not be completed safely"); }
  finally {
    if (measured) console.log(JSON.stringify({ type: "browser_takeover.action", action: measured, ms: Date.now() - started, cdp_calls: cdpCalls, image_base64_bytes: imageBytes, jpeg: touch.jpeg === true }));
    if (sid && !cdp.attachTarget) {
      try { await cdp.send("Target.detachFromTarget", { sessionId: sid }); }
      catch { /* Caller owns the private connection and lease cleanup. */ }
    }
  }
}

/** Release only browser input/emulation state; never capture a final private frame.
 * Finish must remain possible after navigation, expiry or provider failure. */
export async function releasePrivateVaultTakeover(cdp: Pick<PrivateBrowserCdp, "send"> & Partial<Pick<PrivateBrowserCdp, "attachTarget">>, targetId: string): Promise<void> {
  let sid: string | undefined;
  try {
    const attached = cdp.attachTarget ? await cdp.attachTarget(targetId) : await cdp.send("Target.attachToTarget", {targetId,flatten:true});
    if (typeof attached?.sessionId !== "string") return;
    sid = attached.sessionId;
    for (const [method, params] of [
      ["Input.dispatchTouchEvent", {type:"touchCancel",touchPoints:[]}],
      ["Emulation.clearDeviceMetricsOverride", {}],
      ["Emulation.setTouchEmulationEnabled", {enabled:false}],
    ] as const) {
      try { await cdp.send(method, params, sid); } catch { /* Best effort cleanup; never replay user input. */ }
    }
  } catch { /* User can always relinquish control, including an unavailable page. */ }
  finally { if (sid && !cdp.attachTarget) { try { await cdp.send("Target.detachFromTarget", {sessionId:sid}); } catch {} } }
}

// Kept entirely in an isolated world. Neither DOM handles nor field values leave
// this world during discovery. The host remembers only a document/context binding.
const NATIVE_FORM_VISIBLE = `e => (e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement || controls && e instanceof HTMLSelectElement && !e.multiple && e.options.length <= 200 && [...e.options].some(o => !o.disabled && !o.hidden && !o.closest('optgroup[disabled],optgroup[hidden]')))
  && e.ownerDocument === document && e.getRootNode() === document && e.isConnected
  && !e.disabled && !e.matches(':disabled') && !e.readOnly
  && (e instanceof HTMLTextAreaElement || controls && e instanceof HTMLSelectElement || ["text","search","email","url","tel","number","password",...(controls ? ["checkbox"] : [])].includes(e.type))
  && !e.closest('[inert],[hidden],[aria-hidden="true"]')
  && e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})
  && (() => {
    const r = e.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    if (r.right <= 0 || r.bottom <= 0 || r.left >= innerWidth || r.top >= innerHeight) return controls;
    const x = (Math.max(0,r.left)+Math.min(innerWidth,r.right))/2, y = (Math.max(0,r.top)+Math.min(innerHeight,r.bottom))/2;
    return (controls || r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight)
      && e.contains(document.elementFromPoint(x,y));
  })()`;
const NATIVE_FIELD_STATE = `e => [e.outerHTML, 'value' in e ? e.value : null,
  'checked' in e ? e.checked : null, e instanceof HTMLSelectElement ? [...e.options].map(o => o.selected) : null]`;
const NATIVE_FIELD_LABEL = `e => [(e.getAttribute('aria-labelledby') || '').trim().split(/\\s+/).slice(0,16)
  .map(id => document.getElementById(id)?.textContent || '').join(' ').trim(), e.getAttribute('aria-label'),
  Array.from(e.labels || [], l => l.textContent || '').join(' ').trim(), e.getAttribute('placeholder')]`;
const NATIVE_FORM_DISCOVER = `function(documentId, origin, refs, hints, controls, selectionId) {
  if (window.top !== window || location.origin !== origin) return null;
  const visible = ${NATIVE_FORM_VISIBLE}, fieldState = ${NATIVE_FIELD_STATE};
  const formState = ${NATIVE_FORM_STATE}, fieldLabel = ${NATIVE_FIELD_LABEL};
  const selection = selectionId ? globalThis.__nanocodexNativeSelection : null;
  if (selectionId && (!selection || selection.id !== selectionId || selection.document !== document || selection.href !== location.href || !selection.valid()
    || !selection.entries.every(b => visible(b.el) && JSON.stringify(fieldState(b.el)) === b.state && (b.el.form || b.el.closest('form')) === b.form && formState(b.form) === b.formState))) return {stale:true};
  const entries = [], fields = [];
  const candidates = selection ? selection.entries.map(b => b.el) : document.querySelectorAll(controls ? 'input,textarea,select' : 'input,textarea');
  for (const e of candidates) {
    if (entries.length === refs.length) break;
    if (!visible(e)) continue;
    const ref = refs[entries.length], multiline = e instanceof HTMLTextAreaElement;
    const type = e instanceof HTMLSelectElement ? 'select' : multiline || e.type === 'search' ? 'text' : e.type;
    const labelledBy = (e.getAttribute('aria-labelledby') || '').trim().split(/\\s+/).slice(0,16)
      .map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
    const label = (selection?.entries[entries.length].requestedLabel || labelledBy || e.getAttribute('aria-label') || Array.from(e.labels || [], l => l.textContent || '').join(' ').trim()
      || e.getAttribute('placeholder') || (type === 'password' ? 'Password' : 'Field ' + (entries.length + 1)))
      .replace(/[\\u0000-\\u001f\\u007f]/g, ' ').slice(0,160);
    // Export only recognized purpose hints, never arbitrary attribute contents.
    // A trailing webauthn token is not an assertion of passkey support.
    const tokens = (e.autocomplete || '').toLowerCase().trim().split(/\\s+/);
    if (tokens[tokens.length - 1] === 'webauthn') tokens.pop();
    const autocomplete = tokens[tokens.length - 1], inputmode = e.inputMode;
    entries.push({ref,element:e,type:e.type,form:e.form,name:e.name,autocomplete:e.autocomplete,inputmode:e.inputMode,
      markup:e.outerHTML,labelState:JSON.stringify(fieldLabel(e)),state:JSON.stringify(fieldState(e)),formState:formState(e.form)});
    fields.push({ref,label,type,multiline,
      ...(type === 'select' ? {options:[...e.options].flatMap((o,index) => o.disabled || o.hidden || o.closest('optgroup[disabled],optgroup[hidden]') ? [] : [{index,label:o.label.replace(/[\\u0000-\\u001f\\u007f]/g, ' ').slice(0,160)}])} : {}),
      ...(type === 'checkbox' ? {checked:e.checked} : {}),
      ...(hints && ${JSON.stringify(NATIVE_AUTOCOMPLETE)}.includes(autocomplete) ? {autocomplete} : {}),
      ...(hints && ${JSON.stringify(NATIVE_INPUTMODES)}.includes(inputmode) ? {inputmode} : {})});
  }
  globalThis.__nanocodexNativeForm = {documentId,document,origin,href:location.href,entries,controls,selectionId};
  return {fields,...(selection?.reason ? {reason:selection.reason} : {})};
}`;
const NATIVE_FORM_FILL = `function(documentId, origin, fields, saveValues) {
  const bound = globalThis.__nanocodexNativeForm;
  delete globalThis.__nanocodexNativeForm;
  if (!bound || bound.documentId !== documentId || bound.document !== document || bound.href !== location.href
    || bound.origin !== origin || location.origin !== origin || window.top !== window) return false;
  const selection = bound.selectionId ? globalThis.__nanocodexNativeSelection : null;
  if (bound.selectionId && (!selection || selection.id !== bound.selectionId || !selection.valid())) return false;
  const controls = bound.controls, visible = ${NATIVE_FORM_VISIBLE}, fieldState = ${NATIVE_FIELD_STATE};
  const formState = ${NATIVE_FORM_STATE}, fieldLabel = ${NATIVE_FIELD_LABEL};
  const valid = b => b && visible(b.element) && b.element.type === b.type
    && b.element.form === b.form && b.element.name === b.name && JSON.stringify(fieldLabel(b.element)) === b.labelState
    && b.element.autocomplete === b.autocomplete && b.element.inputMode === b.inputmode && location.origin === origin
    && (!b.form || JSON.stringify(JSON.parse(formState(b.form))[0]) === JSON.stringify(JSON.parse(b.formState)[0]));
  const selected = fields.map(f => ({field:f,binding:bound.entries.find(b => b.ref === f.ref)}));
  const validValue = (e, value) => {
    if (e instanceof HTMLSelectElement) {
      if (!/^(0|[1-9][0-9]{0,2})$/.test(value)) return false;
      const o = e.options[Number(value)];
      return !!o && !o.disabled && !o.hidden && !o.closest('optgroup[disabled],optgroup[hidden]');
    }
    return e.type !== 'checkbox' || ['true','false'].includes(value);
  };
  if (!selected.every(s => valid(s.binding) && validValue(s.binding.element,s.field.value)
    && (!controls || JSON.stringify(fieldState(s.binding.element)) === s.binding.state && formState(s.binding.form) === s.binding.formState))) return false;
  // Only the submitted controls may advance the selected sheet's baseline.
  // An input handler that changes hidden state or destinations must stale it.
  const expectedForms = new Map();
  if (selection) for (const b of selection.entries) if (b.form && !expectedForms.has(b.form))
    expectedForms.set(b.form, {state:JSON.parse(b.formState), elements:[...b.form.elements]});
  for (const {field,binding} of selected) {
    if (!valid(binding) || controls && binding.element.outerHTML !== binding.markup) return false;
    const e = binding.element;
    if (e.type === 'checkbox') {
      const checked = field.value === 'true';
      // Frameworks such as React derive controlled checkbox changes from click.
      // Native activation dispatches the matching input/change events once;
      // this is a field operation, never a coordinate click or form submission.
      if (e.checked !== checked) HTMLElement.prototype.click.call(e);
      if (!valid(binding) || e.checked !== checked) return false;
      continue;
    }
    const prototype = e instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : e instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    const property = e instanceof HTMLSelectElement ? 'selectedIndex' : 'value';
    const value = e instanceof HTMLSelectElement ? Number(field.value) : field.value;
    Object.getOwnPropertyDescriptor(prototype, property).set.call(e, value);
    e.dispatchEvent(new Event('input', {bubbles:true,composed:true}));
    if (!valid(binding)) return false;
    e.dispatchEvent(new Event('change', {bubbles:true}));
  }
  // Event handlers may clear an earlier field or change the destination. Never
  // acknowledge or save a batch unless every submitted value remains filled.
  if (!selected.every(({field,binding}) => valid(binding) && (binding.element instanceof HTMLSelectElement
    ? binding.element.selectedIndex === Number(field.value)
    : binding.element.type === 'checkbox' ? binding.element.checked === (field.value === 'true')
    : binding.element.value === (() => {
      if (binding.element instanceof HTMLTextAreaElement) return field.value.replace(/\\r\\n?/g, '\\n');
      const value = field.value.replace(/[\\r\\n]/g, '');
      const trim = s => s.replace(/^[\\t\\n\\f\\r ]+|[\\t\\n\\f\\r ]+$/g, '');
      return binding.element.type === 'email' && binding.element.multiple ? value.split(',').map(trim).join(',')
        : ['email','url'].includes(binding.element.type) ? trim(value) : value;
    })()))) return false;
  if (selection) {
    for (const b of selection.entries) {
      const previous = JSON.parse(b.state), current = fieldState(b.el);
      if (previous[0] !== current[0]) continue;
      b.state = JSON.stringify(current);
      if (!b.form) continue;
      const expected = expectedForms.get(b.form);
      for (const {binding} of selected) {
        const index = expected.elements.indexOf(binding.element);
        if (index >= 0 && expected.state[1][index][0] === binding.element.outerHTML)
          expected.state[1][index] = fieldState(binding.element);
      }
      if (JSON.stringify(expected.state) === formState(b.form)) b.formState = formState(b.form);
    }
  }
  // Select indices are human transport values. Only trusted autosave receives
  // the selected option value; it must never become a model/tool result.
  if (saveValues) {
    const saved = selected.map(({field,binding}) => ({ref:field.ref,value:binding.element.type === 'checkbox' ? field.value : binding.element.value}));
    if (saved.some(f => f.value.length > 4096)) return false;
    return {filled:true,fields:saved};
  }
  return true;
}`;
