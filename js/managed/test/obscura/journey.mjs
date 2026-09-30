async function journey(call, events, checks) {
  const eq = (name, a, b) => {
    const pass = JSON.stringify(a) === JSON.stringify(b);
    checks.push({ name, actual: a, expected: b, pass });
    if (!pass)
      throw new Error(
        name + ": " + JSON.stringify(a) + " != " + JSON.stringify(b),
      );
  };
  const { targetId } = await call("Target.createTarget", {
    url: "about:blank",
  });
  const { sessionId } = await call("Target.attachToTarget", {
    targetId,
    flatten: true,
  });
  const page = (m, p = {}) => call(m, p, sessionId);
  await page("Page.enable");
  await page("Runtime.enable");
  await page("Page.navigate", { url: "https://fixture.example.com/parent" });
  const evalAt = (expression, contextId) =>
    page("Runtime.evaluate", { expression, contextId, returnByValue: true });
  let tree;
  for (let i = 0; i < 50; i++) {
    tree = await page("Page.getFrameTree");
    if (tree.frameTree.childFrames.length) break;
    await new Promise((r) => setTimeout(r, 10));
  }
  eq("iframe has actual frame", tree.frameTree.childFrames.length, 1);
  const childId = tree.frameTree.childFrames[0].frame.id;
  const child = events
    .filter(
      (e) =>
        e.method === "Runtime.executionContextCreated" &&
        e.params.context.auxData.frameId === childId,
    )
    .at(-1)?.params.context.id;
  eq("child default context announced", typeof child, "number");
  eq(
    "script executes and load event fires",
    (await evalAt("scriptResult")).result.value,
    { executed: 1, loaded: 1 },
  );
  eq(
    "child default evaluation selects child",
    (await evalAt("document.title", child)).result.value,
    "Child",
  );
  eq(
    "iframe onload reaches child listeners after external scripts",
    (
      await page("Runtime.evaluate", {
        expression:
          'new Promise((resolve,reject)=>{const end=Date.now()+2000;function check(){if(globalThis.frameHandshake)return resolve(frameHandshake);if(Date.now()>end)return reject(new Error("No frame handshake"));setTimeout(check,10)}check()})',
        awaitPromise: true,
        returnByValue: true,
      })
    ).result.value,
    { data: "child-ready", stable: true },
  );
  eq(
    "iframe keeps initialization fragment",
    (await evalAt("location.hash", child)).result.value,
    "#init=fixture",
  );
  eq(
    "parent remains parent",
    (await evalAt("document.title")).result.value,
    "Parent",
  );
  const called = await page("Runtime.callFunctionOn", {
    executionContextId: child,
    functionDeclaration:
      'function(value){document.querySelector("input").value=value;return document.title}',
    arguments: [{ value: "child-edited" }],
    returnByValue: true,
  });
  eq("callFunctionOn uses child", called.result.value, "Child");
  eq(
    "parent input unchanged",
    (await evalAt('document.querySelector("input").value')).result.value,
    "parent",
  );
  const doc = await page("DOM.getDocument", { depth: -1, pierce: true });
  const flat = [];
  function walk(n) {
    flat.push(n);
    n.children?.forEach(walk);
    if (n.contentDocument) walk(n.contentDocument);
  }
  walk(doc.root);
  const input = flat.find(
    (n) => n.nodeName === "INPUT" && n.attributes.includes("child-input"),
  );
  eq("DOM pierces real child", !!input, true);
  eq(
    "unloaded iframe never points back at the top document",
    flat
      .filter((n) => n.nodeName === "IFRAME" && n.attributes.includes("blank"))
      .every((n) => !n.contentDocument),
    true,
  );
  const childDoc = flat.find(
    (n) => n.documentURL === "https://child.example.com/frame#init=fixture",
  );
  const q = await page("DOM.querySelector", {
    nodeId: childDoc.nodeId,
    selector: "#child-input",
  });
  eq("child scoped DOM query", q.nodeId, input.nodeId);
  await evalAt(
    'document.querySelector("input").addEventListener("input",e=>globalThis.inputEcho=[e.data,e.inputType,e.isTrusted]);document.querySelector("input").addEventListener("beforeinput",e=>{if(e.data==="blocked")e.preventDefault()})',
    child,
  );
  await page("DOM.focus", { nodeId: input.nodeId });
  await page("Input.insertText", { text: "-typed" });
  eq(
    "typing reaches child",
    (await evalAt('document.querySelector("input").value', child)).result.value,
    "child-edited-typed",
  );
  eq(
    "typing delivers browser input metadata",
    (await evalAt("inputEcho", child)).result.value,
    ["-typed", "insertText", true],
  );
  await page("Input.insertText", { text: "blocked" });
  eq(
    "cancelled beforeinput prevents editing",
    (await evalAt('document.querySelector("input").value', child)).result.value,
    "child-edited-typed",
  );
  const resolved = await page("DOM.resolveNode", { nodeId: input.nodeId });
  const value = await page("Runtime.callFunctionOn", {
    objectId: resolved.object.objectId,
    functionDeclaration: "function(){return this.value}",
    returnByValue: true,
  });
  eq(
    "resolved child node remote object",
    value.result.value,
    "child-edited-typed",
  );
  await page("Runtime.releaseObject", { objectId: resolved.object.objectId });
  const plain = await page("Runtime.evaluate", {
    expression: "({answer:42})",
    awaitPromise: true,
    returnByValue: true,
  });
  eq(
    "awaitPromise accepts a plain object without releasing it early",
    plain.result.value,
    { answer: 42 },
  );
  const promise = await page("Runtime.evaluate", {
    expression: 'new Promise(r=>setTimeout(()=>r("resolved"),10))',
    awaitPromise: true,
    returnByValue: true,
  });
  eq("promise runs jobs and timers", promise.result.value, "resolved");
  const cancelled = await page("Runtime.evaluate", {
    expression:
      'new Promise(resolve=>{const id=setTimeout(()=>resolve("fired"),10);clearTimeout(id);setTimeout(()=>resolve("cancelled"),30)})',
    awaitPromise: true,
    returnByValue: true,
  });
  eq(
    "timer cancellation preserves the host bridge contract",
    cancelled.result.value,
    "cancelled",
  );
  const err = await page("Runtime.evaluate", {
    expression: 'throw new Error("fixture error")',
    returnByValue: true,
  });
  eq("exception reported", !!err.exceptionDetails, true);
  const largeScript = await page("Runtime.evaluate", {
    expression:
      'new Promise(resolve=>{const s=document.createElement("script");s.src="/large.js";s.onload=()=>resolve(globalThis.largeScriptLoaded===true);s.onerror=()=>resolve("error");document.head.appendChild(s)})',
    awaitPromise: true,
    returnByValue: true,
  });
  eq(
    "large script executes after Wasm memory growth",
    largeScript.result.value,
    true,
  );
  const oversized = await page("Runtime.evaluate", {
    expression:
      'fetch("/oversized").then(()=>false,e=>String(e).includes("16 MiB"))',
    awaitPromise: true,
    returnByValue: true,
  });
  eq(
    "streamed response exceeding 16 MiB is rejected",
    oversized.result.value,
    true,
  );
  await evalAt(
    '(()=>{const e=document.createElement("input");e.id="dynamic-input";e.name="dynamic";e.setAttribute("data-fixture","before\\u0000after");document.body.appendChild(e)})()',
  );
  const dynamic = await page("DOM.querySelector", {
    nodeId: doc.root.nodeId,
    selector: "input[name=dynamic]",
  });
  const dynamicAttributes = dynamic.nodeId
    ? await page("DOM.getAttributes", { nodeId: dynamic.nodeId })
    : { attributes: [] };
  eq(
    "dynamic attributes including NUL reach the Wasm DOM",
    dynamicAttributes.attributes,
    [
      "id",
      "dynamic-input",
      "name",
      "dynamic",
      "data-fixture",
      "before\u0000after",
    ],
  );
  const noContent = await page("Runtime.evaluate", {
    expression: 'fetch("/no-content").then(r=>r.status)',
    awaitPromise: true,
    returnByValue: true,
  });
  eq("bodyless HTTP 204 is preserved", noContent.result.value, 204);
  const asyncEval = async (expression) =>
    (
      await page("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
      })
    ).result.value;
  eq(
    "stylesheet load retains CSSOM and edits",
    await asyncEval(
      'new Promise(resolve=>{const l=document.createElement("link");l.rel="stylesheet";l.href="/fixture.css";l.onload=()=>{const before=l.sheet.cssRules[0].selectorText;l.sheet.insertRule(".inserted { color: blue; }",1);const after=l.sheet.cssRules[1].selectorText;resolve([before,after,l.sheet.cssRules.length])};l.onerror=()=>resolve("error");document.head.appendChild(l)})',
    ),
    [".fixture", ".inserted", 2],
  );
  eq(
    "cross-origin stylesheet loads but protects rules",
    await asyncEval(
      'new Promise(resolve=>{const l=document.createElement("link");l.rel="stylesheet";l.href="https://child.example.com/fixture.css";l.onload=()=>{try{l.sheet.cssRules;resolve("exposed")}catch(e){resolve(e.name)}};l.onerror=()=>resolve("error");document.head.appendChild(l)})',
    ),
    "SecurityError",
  );
  eq(
    "failed stylesheet emits error",
    await asyncEval(
      'new Promise(resolve=>{const l=document.createElement("link");l.rel="stylesheet";l.href="/missing.css";l.onload=()=>resolve("load");l.onerror=()=>resolve("error");document.head.appendChild(l)})',
    ),
    "error",
  );
  eq(
    "CORS allows server-authorized reads and filters headers",
    await asyncEval(
      'fetch("https://child.example.com/cors-read").then(async r=>({type:r.type,data:await r.json(),visible:r.headers.get("x-visible"),hidden:r.headers.get("x-hidden")}))',
    ),
    {
      type: "cors",
      data: {
        method: "GET",
        origin: "https://fixture.example.com",
        header: null,
        body: null,
      },
      visible: "public",
      hidden: null,
    },
  );
  eq(
    "CORS preflight authorizes a custom-header write",
    await asyncEval(
      'fetch("https://child.example.com/cors-write",{method:"PUT",headers:{"x-fixture":"test"},body:"synthetic"}).then(r=>r.json())',
    ),
    {
      method: "PUT",
      origin: "https://fixture.example.com",
      header: "test",
      body: "synthetic",
    },
  );
  eq(
    "CORS denies origin, mode, credentials and preflight violations",
    await asyncEval(
      'Promise.all([fetch("https://child.example.com/cors-wrong"),fetch("https://child.example.com/cors-read",{mode:"same-origin"}),fetch("https://child.example.com/cors-read",{mode:"no-cors"}),fetch("https://child.example.com/cors-wildcard",{credentials:"include"}),fetch("https://child.example.com/cors-read",{credentials:"include"}),fetch("https://child.example.com/cors-deny",{method:"PUT",headers:{"x-fixture":"test"},body:"denied"})].map(p=>p.then(()=>false,e=>e instanceof Error)))',
    ),
    [true, true, true, true, true, true],
  );
  eq(
    "failed preflight never sends the write",
    await asyncEval('fetch("/cors-stats").then(r=>r.json())'),
    { deniedWrites: 0 },
  );
  eq(
    "CORS accepts wildcard without credentials and explicit credentials permission",
    await asyncEval(
      'Promise.all([fetch("https://child.example.com/cors-wildcard"),fetch("https://child.example.com/cors-credentials",{credentials:"include"})].map(p=>p.then(r=>r.status)))',
    ),
    [200, 200],
  );
  let rejected = false;
  try {
    await page("Page.captureScreenshot");
  } catch (e) {
    rejected = /does not implement/.test(String(e));
  }
  eq("unsupported renderer fails explicitly", rejected, true);
  await page("Page.navigate", {
    url: "https://fixture.example.com/redirect-fragment#kept",
  });
  eq(
    "navigation redirects inherit absent fragments",
    (await evalAt("location.hash")).result.value,
    "#kept",
  );
  await page("Page.navigate", {
    url: "https://fixture.example.com/redirect-new-fragment#discarded",
  });
  eq(
    "navigation redirects replace explicit fragments",
    (await evalAt("location.hash")).result.value,
    "#replaced",
  );
  await page("Page.navigate", { url: "about:blank" });
  rejected = false;
  try {
    await evalAt("document.title", child);
  } catch {
    rejected = true;
  }
  eq("stale child context rejected", rejected, true);
  await call("Target.closeTarget", { targetId });
  eq(
    "target close cleans up",
    (await call("Target.getTargets")).targetInfos.length,
    0,
  );
  return checks;
}
const fixtures = {
  "https://fixture.example.com/parent":
    '<!doctype html><title>Parent</title><input id="parent-input" value="parent"><iframe id="blank" src="about:blank"></iframe><iframe src="https://child.example.com/frame#init=fixture" onload="this.contentWindow.postMessage(&quot;parent-ready&quot;,&quot;https://child.example.com&quot;)"></iframe><script>globalThis.capturedWindow=document.querySelectorAll("iframe")[1].contentWindow;addEventListener("message",e=>{if(e.origin==="https://child.example.com")globalThis.frameHandshake={data:e.data,stable:e.source===capturedWindow}});globalThis.scriptResult={executed:0,loaded:0};const s=document.createElement("script");s.src="/external.js";s.async=true;s.addEventListener("load",()=>scriptResult.loaded++);document.head.appendChild(s)<\/script>',
  "https://fixture.example.com/external.js": "scriptResult.executed++",
  "https://child.example.com/frame":
    '<!doctype html><title>Child</title><input id="child-input" value="child"><script src="/frame-listener.js"></script>',
};
export { fixtures, journey };
