import { isBrowserVaultOrigin, type BrowserVaultIdentity, type PrivateBrowserCdp } from "./browser-vault";

export type BrowserVaultTakeoverAction =
  | { action: "observe"; viewport?: { width: number; height: number; mobile: boolean } }
  | { action: "click"; x: number; y: number }
  | { action: "type"; text: string }
  | { action: "edit"; delete_backward: number; text: string }
  | { action: "touch"; phase: "start" | "move" | "end" | "cancel"; x?: number; y?: number }
  | { action: "key"; key: "Enter" | "Tab" | "Backspace" | "Escape" }
  | { action: "scroll"; delta_y: number };
export type BrowserVaultTouchState = { active?: boolean; uncertain?: boolean };
export type BrowserVaultKeyboard = { type: "text" | "email" | "url" | "tel" | "number" | "password"; multiline: boolean };
export type BrowserVaultTakeoverResult = { status: "active"; image: string; width: number; height: number; keyboard?: BrowserVaultKeyboard; inputs?: (BrowserVaultKeyboard & { x: number; y: number; width: number; height: number })[] };

const MAX_IMAGE_BASE64 = 8 * 1024 * 1024;
const keys = { Enter: 13, Tab: 9, Backspace: 8, Escape: 27 } as const;
export function validateBrowserVaultTakeoverAction(value: BrowserVaultTakeoverAction) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
  let allowed: string[];
  switch (value.action) {
    case "observe":
      if (value.viewport !== undefined) {
        const v = value.viewport;
        if (!v || typeof v !== "object" || Array.isArray(v) || typeof v.mobile !== "boolean"
          || ![v.width,v.height].every(n => Number.isInteger(n) && n >= 240 && n <= 1920)
          || Object.keys(v).some(k => !["width","height","mobile"].includes(k))) throw new Error();
      }
      allowed = ["action", "viewport"]; break;
    case "click":
      if (![value.x, value.y].every(n => Number.isFinite(n) && n >= 0 && n <= 1)) throw new Error();
      allowed = ["action", "x", "y"]; break;
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
): Promise<BrowserVaultTakeoverResult> {
  let sid: string | undefined;
  try {
    validateBrowserVaultTakeoverAction(action);
    if (action.action === "touch") {
      if (action.phase !== "cancel" && (touch.uncertain || (action.phase === "start" ? touch.active : !touch.active))) throw new Error();
    } else if (action.action !== "observe" && (touch.active || touch.uncertain)) throw new Error();
    if (!identity || !isBrowserVaultOrigin(identity.expected_origin)
      || typeof identity.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(identity.vault_id)
      || typeof identity.target_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(identity.target_id)) throw new Error();
    const sameOrigin = (value: unknown) => {
      if (typeof value !== "string") throw new Error();
      const url = new URL(value);
      if (url.protocol !== "https:" || url.origin !== identity.expected_origin || url.username || url.password) throw new Error();
    };
    const checkTarget = async () => {
      const { targetInfo } = await cdp.send("Target.getTargetInfo", { targetId: identity.target_id });
      if (targetInfo?.type !== "page" || (targetInfo.targetId !== undefined && targetInfo.targetId !== identity.target_id)) throw new Error();
      sameOrigin(targetInfo.url);
    };
    await checkTarget();
    const attached = cdp.attachTarget ? await cdp.attachTarget(identity.target_id) : await cdp.send("Target.attachToTarget", { targetId: identity.target_id, flatten: true });
    if (typeof attached?.sessionId !== "string" || !attached.sessionId) throw new Error();
    sid = attached.sessionId;
    let frameId = "";
    const check = async () => {
      await checkTarget();
      const tree = await cdp.send("Page.getFrameTree", {}, sid);
      const frame = tree?.frameTree?.frame;
      if (!frame || frame.parentId || typeof frame.id !== "string" || !frame.id) throw new Error();
      sameOrigin(frame.url);
      frameId = frame.id;
    };
    await check();
    if (action.action === "observe") {
      // Observation is explicit recovery after an ambiguous gesture, never a replay.
      await check();
      // Chrome rejects touchCancel when no touch sequence has started.
      if (touch.active || touch.uncertain) {
        try { await cdp.send("Input.dispatchTouchEvent", { type: "touchCancel", touchPoints: [] }, sid); }
        catch (error) {
          // A replaced channel can have no finger despite uncertain lease state.
          // Only Chrome's specific absent-sequence rejection confirms recovery.
          if (!(error instanceof Error) || !/(?:^|: )Must send a TouchStart first to start a new touch\.$/.test(error.message)) throw error;
        }
      }
      touch.active = false; touch.uncertain = false;
      await check();
      if (restoreViewport) {
        await cdp.send("Emulation.clearDeviceMetricsOverride", {}, sid);
        await check();
        await cdp.send("Emulation.setTouchEmulationEnabled", { enabled: false }, sid);
        await check();
      } else if (action.viewport) {
        await cdp.send("Emulation.setDeviceMetricsOverride", { ...action.viewport, deviceScaleFactor: 1 }, sid);
        await check();
        await cdp.send("Emulation.setTouchEmulationEnabled", { enabled: action.viewport.mobile, maxTouchPoints: 1 }, sid);
        await check();
      }
    }
    const metrics = await cdp.send("Page.getLayoutMetrics", {}, sid);
    const viewport = metrics?.cssLayoutViewport;
    const width = viewport?.clientWidth, height = viewport?.clientHeight;
    if (![width, height].every(n => typeof n === "number" && Number.isInteger(n) && n > 0 && n <= 8192)
      || width * height > 16_777_216) throw new Error();
    const input = async (method: string, params: unknown) => {
      await check();
      await cdp.send(method, params, sid);
      await check();
    };
    if (action.action === "touch") {
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
      for (let i = 0; i < action.delete_backward; i++) {
        await input("Input.dispatchKeyEvent", { type: "keyDown", key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 });
        await input("Input.dispatchKeyEvent", { type: "keyUp", key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 });
      }
      if (action.text) await input("Input.insertText", { text: action.text });
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
    await check();
    // Fixed isolated-world code returns only an allowlisted descriptor, never field values.
    let keyboard: BrowserVaultKeyboard | undefined;
    let inputs: BrowserVaultTakeoverResult["inputs"];
    try {
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
    } catch { /* Optional focus metadata is unavailable; never forward provider errors. */ }
    await check();
    const screenshot = await cdp.send("Page.captureScreenshot", { format: "png", fromSurface: true, captureBeyondViewport: false }, sid);
    await check();
    const data = screenshot?.data;
    if (typeof data !== "string" || data.length < 44 || data.length > MAX_IMAGE_BASE64
      || data.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)) throw new Error();
    const header = atob(data.slice(0, 44));
    if (header.slice(0, 8) !== "\x89PNG\r\n\x1a\n" || header.slice(12, 16) !== "IHDR") throw new Error();
    const dimension = (offset: number) => [...header.slice(offset, offset + 4)].reduce((n, c) => n * 256 + c.charCodeAt(0), 0);
    const imageWidth = dimension(16), imageHeight = dimension(20);
    if (!imageWidth || !imageHeight || imageWidth > 8192 || imageHeight > 8192 || imageWidth * imageHeight > 16_777_216) throw new Error();
    return { status: "active", image: `data:image/png;base64,${data}`, width: imageWidth, height: imageHeight, ...(keyboard ? { keyboard } : {}), ...(inputs ? { inputs } : {}) };
  } catch { throw new Error("Private browser takeover could not be completed safely"); }
  finally {
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
