let app = await cua.getApp("fixture://native");
await app.selectText(1, "alpha", {prefix: "blue ", suffix: " green"});
await app.typeText("β終🧪");
await app.click(2);
await app.getAXState();
let screenshot = await app.getScreenshot({emit: false});
nodeRepl.write({byteLength: screenshot.length, format: "synthetic PNG"});
await nodeRepl.emitImage(screenshot);
