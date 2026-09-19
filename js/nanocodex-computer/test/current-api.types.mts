import type { Cua, Tab } from "../api.d.mts";
declare const cua: Cua;
const app = await cua.getApp!({ windowId: 7 });
await app.typeText("native text");
await app.scroll([10, 20], "down", { pixels: 40 });
await cua.listWindows!();
const tab = await cua.getTab!({ url: "https://example.test" }, { browser: "chrome" });
await tab.typeText(null, "browser text");
await tab.pressKey(7, "Enter");
await tab.paste(7, "browser text", { format: "text" });
// @ts-expect-error Targeted browser input requires an index or explicit null.
await tab.typeText("browser text");
const browser = await cua.getBrowser!({ extensionInstanceId: "fixture" });
const decorated: Tab = await browser.tabs.get("fixture");
await decorated.getScreenshot();
