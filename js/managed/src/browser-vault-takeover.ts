import { isBrowserVaultOrigin, type BrowserVaultIdentity, type PrivateBrowserCdp } from "./browser-vault";

export type BrowserVaultTakeoverAction =
  | { action: "observe" }
  | { action: "click"; x: number; y: number }
  | { action: "type"; text: string }
  | { action: "key"; key: "Enter" | "Tab" | "Backspace" | "Escape" }
  | { action: "scroll"; delta_y: number };
export type BrowserVaultTakeoverResult = { status: "active"; image: string; width: number; height: number };

const MAX_IMAGE_BASE64 = 8 * 1024 * 1024;
const keys = { Enter: 13, Tab: 9, Backspace: 8, Escape: 27 } as const;
function validateAction(value: BrowserVaultTakeoverAction) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
  let allowed: string[];
  switch (value.action) {
    case "observe": allowed = ["action"]; break;
    case "click":
      if (![value.x, value.y].every(n => Number.isFinite(n) && n >= 0 && n <= 1)) throw new Error();
      allowed = ["action", "x", "y"]; break;
    case "type":
      if (typeof value.text !== "string" || !value.text.length || value.text.length > 512) throw new Error();
      allowed = ["action", "text"]; break;
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
 * all model access, and closes the private connection after each operation. The
 * quarantined browser and lease remain for explicit refresh/recovery. Origin checks
 * bracket each input and screenshot; they cannot make browser navigation atomic.
 */
export async function privateVaultTakeover(
  cdp: Pick<PrivateBrowserCdp, "send">, identity: BrowserVaultIdentity, action: BrowserVaultTakeoverAction,
): Promise<BrowserVaultTakeoverResult> {
  let sid: string | undefined;
  try {
    validateAction(action);
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
    const attached = await cdp.send("Target.attachToTarget", { targetId: identity.target_id, flatten: true });
    if (typeof attached?.sessionId !== "string" || !attached.sessionId) throw new Error();
    sid = attached.sessionId;
    const check = async () => {
      await checkTarget();
      const tree = await cdp.send("Page.getFrameTree", {}, sid);
      const frame = tree?.frameTree?.frame;
      if (!frame || frame.parentId || typeof frame.id !== "string" || !frame.id) throw new Error();
      sameOrigin(frame.url);
    };
    await check();
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
    if (action.action === "click") {
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
    return { status: "active", image: `data:image/png;base64,${data}`, width: imageWidth, height: imageHeight };
  } catch { throw new Error("Private browser takeover could not be completed safely"); }
  finally {
    if (sid) {
      try { await cdp.send("Target.detachFromTarget", { sessionId: sid }); }
      catch { /* Caller owns the private connection and lease cleanup. */ }
    }
  }
}
