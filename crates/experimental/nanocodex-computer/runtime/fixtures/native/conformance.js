// build.py replaces this token with the exact owned fixture bundle path.
const appId = "SKYRE_OWNED_FIXTURE_APP";
// Let the external controller finish observing the Run button before this
// helper acquires focus for native input. This is fixture coordination only;
// every operation below still checks actual completion and target identity.
await new Promise(resolve => setTimeout(resolve, 3000));
var app = await cua.getApp(appId);

function assert(condition, name) {
  if (!condition) throw new Error("ASSERTION FAILED: " + name);
  nodeRepl.write({case: name, pass: true});
}

// Use only public AX text and IDs. Never use the implementation's private tree.
async function control(identifier) {
  const text = await app.getAXState({disableDiffing: true, emit: false});
  const marker = ", ID: " + identifier;
  const matches = new Map();
  for (const line of text.split("\n")) {
    const position = line.indexOf(marker);
    if (position < 0 || !/^(?:,| |$)/.test(line.slice(position + marker.length))) continue;
    const numbered = line.match(/^\s*(\d+)\s+(.+)$/);
    if (!numbered) continue;
    // Unlabelled controls can begin with Value immediately after the role,
    // and the public renderer may place ID before or after it. These fixture
    // values contain no metadata delimiters; do not depend on field order.
    const valueMatch = line.match(/(?:^|[ ,])Value: (.*?)(?=, (?:ID: |Secondary Actions: |Help: )|$)/);
    matches.set(Number(numbered[1]), {
      id: Number(numbered[1]), line: numbered[2],
      value: valueMatch ? valueMatch[1] : null,
      settable: /\([^)]*\bsettable\b[^)]*\)/.test(numbered[2]),
    });
  }
  if (matches.size !== 1) throw new Error("Expected one owned fixture control: " + identifier + "; found " + matches.size);
  return [...matches.values()][0];
}

async function fieldDiagnostic(stage) {
  const field = await control("skyre.test.input");
  nodeRepl.write({diagnostic: stage, value: field.value});
  return field;
}

function jpegHeader(bytes) {
  // The recovered public default is JPEG; PNG is an explicit native flag.
  // See tests/native_screenshot.rs and its source-pinned encoding oracle.
  if (bytes.length < 100 || bytes[0] !== 0xff || bytes[1] !== 0xd8 ||
      bytes[bytes.length - 2] !== 0xff || bytes[bytes.length - 1] !== 0xd9) {
    throw new Error('Screenshot JPEG signature missing');
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const frames = [0xc0,0xc1,0xc2,0xc3,0xc5,0xc6,0xc7,0xc9,0xca,0xcb,0xcd,0xce,0xcf];
  let offset = 2;
  while (offset < bytes.length - 2) {
    if (bytes[offset++] !== 0xff) throw new Error('Screenshot JPEG marker missing');
    while (offset < bytes.length && bytes[offset] === 0xff) offset++;
    const marker = bytes[offset++];
    if (marker === 0xda || marker === 0xd9) break;
    if (marker === 0x01 || marker >= 0xd0 && marker <= 0xd8) continue;
    if (offset + 2 > bytes.length) throw new Error('Screenshot JPEG marker truncated');
    const length = view.getUint16(offset);
    if (length < 2 || offset + length > bytes.length) throw new Error('Screenshot JPEG segment truncated');
    if (frames.includes(marker)) {
      if (length < 8) throw new Error('Screenshot JPEG frame truncated');
      const height = view.getUint16(offset + 3), width = view.getUint16(offset + 5);
      if (width === 0 || height === 0) throw new Error('Screenshot JPEG dimensions are empty');
      // Full decoding and target-pixel inspection happen on the retained image;
      // this public-script assertion is only a header/dimensions smoke check.
      return {width,height};
    }
    offset += length;
  }
  throw new Error('Screenshot JPEG frame missing');
}

async function expectSelectionFailure(text, name) {
  const field = await control('skyre.test.input');
  let rejected = false;
  try { await app.selectText(field.id,text); }
  catch (error) {
    if (error.code !== -10005 || error.message !== 'Could not find the requested text to select in the element') throw error;
    rejected = true;
    nodeRepl.write({diagnostic:name,code:error.code,error:error.message});
  }
  assert(rejected,name);
  assert((await control('skyre.test.input')).value === field.value,name + ' preserves field value');
}

const initialScreenshot = await app.getScreenshot({emit: false});
nodeRepl.write({case:'initial screenshot JPEG header and dimensions',pass:true,...jpegHeader(initialScreenshot)});
assert((await control("skyre.test.input")).settable, "public AX tree identifies editable fixture field");
await app.setValue((await control("skyre.test.input")).id, "red alpha blue alpha green");
await app.selectText((await control("skyre.test.input")).id, "alpha", {prefix: "blue ", suffix: " green"});
await app.typeText("β終🧪");
assert((await fieldDiagnostic("after contextual Unicode replacement")).value === "red alpha blue β終🧪 green", "UTF-16 contextual selection and full Unicode typing");

await expectSelectionFailure('absent','missing selection rejects');
await app.setValue((await control("skyre.test.input")).id, "red alpha blue alpha green");
await expectSelectionFailure('alpha','ambiguous selector rejects');

const beforeCount = Number((await control("skyre.test.counter")).line.match(/Count:\s*(\d+)/)[1]);
await app.click((await control("skyre.test.increment")).id);
const afterCount = Number((await control("skyre.test.counter")).line.match(/Count:\s*(\d+)/)[1]);
assert(afterCount === beforeCount + 1, "AXPress invokes fixture callback exactly once");
await app.setValue((await control("skyre.test.slider")).id, "73");
assert(Number((await control("skyre.test.slider")).value) === 73, "numeric AXValue setter");
const beforeCheck = Number((await control("skyre.test.checkbox")).value);
await app.click((await control("skyre.test.checkbox")).id);
assert(Number((await control("skyre.test.checkbox")).value) === 1 - beforeCheck, "checkbox click toggles native value");

await app.setValue((await control("skyre.test.input")).id, "keyboard target");
await app.selectText((await control("skyre.test.input")).id, "keyboard");
await app.pressKey("super+a");
await app.typeText("chord verified");
assert((await fieldDiagnostic("after modifier select-all and typing")).value === "chord verified", "native select-all shortcut dispatch");
for (const [format, text, expected] of [
  ["text", "clipboard β🧪", "clipboard β🧪"],
  ["md", "**bold** and *italic*", "bold and italic"],
  ["html", "<p>hello <b>world</b></p>", "hello world"],
]) {
  await app.setValue((await control("skyre.test.input")).id, "paste target");
  await app.selectText((await control("skyre.test.input")).id, "paste target");
  await app.paste(text, {format});
  assert((await fieldDiagnostic("after " + format + " paste")).value.trim() === expected, "provider-backed " + format + " paste consumed");
}
await app.selectText((await control("skyre.test.input")).id, "hello world");
await app.pressKey("super+a");
await app.typeText("final synthetic value");
assert((await control("skyre.test.input")).value === "final synthetic value", "modifier chord and event replacement");
const shot = await app.getScreenshot({emit: false});
const dimensions = jpegHeader(shot);
nodeRepl.write({case:'final screenshot JPEG header and dimensions',pass:true,...dimensions});
await nodeRepl.emitImage(shot);
nodeRepl.write({case: "native_conformance_complete", pass: true, screenshotBytes: shot.length,...dimensions});
