/** Experimental CDP over genuine Obscura Wasm DOM and isolated QuickJS realms. */
export class ProtocolError extends Error {
  constructor(message, code = -32000) {
    super(message);
    this.code = code;
  }
}
export class ObscuraBrowser {
  constructor({
    createHost,
    fetch,
    onEvent = () => {},
    onCloseTarget = () => {},
    maxTargets = 4,
    maxNodes = 20000,
    evaluationTimeoutMs = 10000,
  }) {
    Object.assign(this, {
      createHost,
      fetch,
      onEvent,
      onCloseTarget,
      maxTargets,
      maxNodes,
      evaluationTimeoutMs,
    });
    this.targets = new Map();
    this.sessions = new Map();
    this.contexts = new Map();
    this.objects = new Map();
    this.nodes = new Map();
    this.next = 1;
    this.closed = false;
  }
  id(prefix) {
    return `${prefix}-${this.next++}`;
  }
  event(method, params, sessionId) {
    this.onEvent({ method, params, ...(sessionId ? { sessionId } : {}) });
  }
  targetInfo(t) {
    return {
      targetId: t.id,
      type: "page",
      title: t.page?.run("document.title") || "",
      url: t.url,
      attached: [...this.sessions.values()].some((s) => s.target === t),
      canAccessOpener: false,
      browserContextId: "default",
    };
  }
  targetFor(sid) {
    const s = this.sessions.get(sid);
    if (!s) throw new ProtocolError("Unknown or detached session");
    return s.target;
  }
  frameId(t, page) {
    return `${t.id}:frame:${page.frameId}`;
  }
  syncContexts(t) {
    if (!t.host) return;
    for (const page of t.host.pages.values()) {
      if ([...this.contexts.values()].some((c) => c.page === page)) continue;
      const id = this.next++;
      this.contexts.set(id, { id, target: t, page });
      for (const [sid, s] of this.sessions)
        if (s.target === t && s.runtime) this.announce(id, sid);
    }
  }
  announce(id, sid) {
    const c = this.contexts.get(id);
    this.event(
      "Runtime.executionContextCreated",
      {
        context: {
          id,
          origin: new URL(c.page.run("location.href")).origin,
          name: "",
          uniqueId: `obscura-${id}`,
          auxData: {
            isDefault: true,
            type: "default",
            frameId: this.frameId(c.target, c.page),
          },
        },
      },
      sid,
    );
  }
  context(t, id) {
    this.syncContexts(t);
    if (id !== undefined) {
      const c = this.contexts.get(id);
      if (!c || c.target !== t)
        throw new ProtocolError(
          "Execution context does not belong to this target",
        );
      return c;
    }
    return [...this.contexts.values()].find(
      (c) => c.target === t && c.page.frameId === 0,
    );
  }
  releaseTarget(t) {
    for (const [id, o] of this.objects)
      if (o.context.target === t) {
        if (o.handle.alive) o.handle.dispose();
        this.objects.delete(id);
      }
    for (const [id, c] of this.contexts)
      if (c.target === t) this.contexts.delete(id);
    for (const [id, n] of this.nodes) if (n.target === t) this.nodes.delete(id);
    for (const [sid, s] of this.sessions)
      if (s.target === t && s.runtime)
        this.event("Runtime.executionContextsCleared", {}, sid);
    t.host?.close();
    t.host = null;
    t.page = null;
    t.focused = null;
  }
  async navigate(t, url) {
    if (url !== "about:blank") {
      const u = new URL(url);
      if (!["http:", "https:"].includes(u.protocol))
        throw new ProtocolError("Only HTTP(S) navigation is supported");
    }
    let html = "",
      resolved = url;
    if (url !== "about:blank") {
      const response = await this.fetch(url, {
        method: "GET",
        kind: "navigation",
      });
      html = await response.text();
      resolved = response.url || url;
    }
    this.releaseTarget(t);
    t.url = resolved;
    t.host = this.createHost(t.id);
    t.page = t.host.createPage({ html, url: resolved });
    t.generation++;
    this.syncContexts(t);
    for (const [sid, s] of this.sessions)
      if (s.target === t && s.page)
        this.event(
          "Page.frameNavigated",
          {
            frame: {
              id: this.frameId(t, t.page),
              loaderId: `${t.id}:${t.generation}`,
              url: resolved,
              domainAndRegistry: "",
              securityOrigin: new URL(resolved).origin,
              mimeType: "text/html",
            },
          },
          sid,
        );
    await t.page.runParserScriptsAsync();
    this.syncContexts(t);
    for (const [sid, s] of this.sessions)
      if (s.target === t && s.page) {
        this.event(
          "Page.domContentEventFired",
          { timestamp: Date.now() / 1000 },
          sid,
        );
        this.event(
          "Page.loadEventFired",
          { timestamp: Date.now() / 1000 },
          sid,
        );
      }
    return {
      frameId: this.frameId(t, t.page),
      loaderId: `${t.id}:${t.generation}`,
    };
  }
  async createTarget(url) {
    if (this.targets.size >= this.maxTargets)
      throw new ProtocolError("Browser target limit reached");
    const t = {
      id: this.id("target"),
      url: "about:blank",
      generation: 0,
      host: null,
      page: null,
    };
    this.targets.set(t.id, t);
    try {
      await this.navigate(t, url);
      return t;
    } catch (e) {
      this.releaseTarget(t);
      this.targets.delete(t.id);
      throw e;
    }
  }
  nodeId(t, page, nid) {
    for (const [id, n] of this.nodes)
      if (n.target === t && n.page === page && n.nid === nid) return id;
    if (this.nodes.size >= this.maxNodes)
      throw new ProtocolError("DOM node limit reached");
    const id = this.next++;
    this.nodes.set(id, { target: t, page, nid });
    return id;
  }
  node(t, id) {
    const n = this.nodes.get(id);
    if (!n || n.target !== t || !t.host.pages.has(n.page.frameId))
      throw new ProtocolError("Unknown or stale DOM node");
    return n;
  }
  nodeExpr(n) {
    return `_wrap(${n.nid})`;
  }
  describeNode(
    t,
    page,
    nid,
    depth = 0,
    pierce = false,
    budget = { left: this.maxNodes },
  ) {
    if (--budget.left < 0)
      throw new ProtocolError("DOM traversal limit reached");
    const command = (c, a = String(nid), b = "") =>
      JSON.parse(page.dom.command(c, a, b));
    const type = command("node_type"),
      name = command("node_name"),
      id = this.nodeId(t, page, nid),
      children = command("child_nodes");
    const node = {
      nodeId: id,
      backendNodeId: id,
      nodeType: type,
      nodeName: name,
      localName: type === 1 ? command("local_name") : "",
      nodeValue: type === 3 || type === 8 ? command("text_content") : "",
      childNodeCount: children.length,
    };
    if (type === 1)
      node.attributes = command("attribute_names").flatMap((k) => [
        k,
        command("get_attribute", String(nid), k) || "",
      ]);
    if (type === 9) {
      node.documentURL = page.run("location.href");
      node.baseURL = page.run("document.baseURI");
      node.xmlVersion = "";
    }
    if (depth !== 0)
      node.children = children.map((c) =>
        this.describeNode(
          t,
          page,
          c,
          depth < 0 ? -1 : depth - 1,
          pierce,
          budget,
        ),
      );
    if (pierce && name === "IFRAME") {
      const childId = page.run(`_wrap(${nid})._frameId`);
      // Zero denotes an iframe without a loaded realm, not the top document.
      const child = childId > 0 ? t.host.pages.get(childId) : undefined;
      if (child && child.parentFrameId === page.frameId) {
        node.frameId = this.frameId(t, child);
        node.contentDocument = this.describeNode(
          t,
          child,
          Number(child.dom.command("document_node_id", "", "")),
          depth < 0 ? -1 : Math.max(0, depth - 1),
          pierce,
          budget,
        );
      }
    }
    return node;
  }
  remote(context, h, byValue = false, group = "") {
    const vm = context.page.vm,
      type = vm.typeof(h);
    let result = { type };
    if (type === "undefined") return result;
    if (type === "string" || type === "boolean") {
      result.value = vm.dump(h);
      return result;
    }
    if (type === "number") {
      const n = vm.getNumber(h);
      if (Number.isFinite(n) && !Object.is(n, -0)) result.value = n;
      else result.unserializableValue = String(Object.is(n, -0) ? "-0" : n);
      return result;
    }
    if (type === "bigint") {
      result.unserializableValue = `${vm.getBigInt(h)}n`;
      return result;
    }
    if (type === "object" && vm.eq(h, vm.null))
      return { type: "object", subtype: "null", value: null };
    if (byValue) {
      const fn = vm.evalCode("(value)=>JSON.stringify(value)");
      if (fn.error) {
        fn.error.dispose();
        throw new ProtocolError("Value serializer unavailable");
      }
      try {
        const value = vm.callFunction(fn.value, vm.undefined, h);
        if (value.error) {
          value.error.dispose();
          throw new ProtocolError("Result cannot be serialized by value");
        }
        try {
          const str = vm.dump(value.value);
          if (str !== undefined) result.value = JSON.parse(str);
        } finally {
          value.value.dispose();
        }
      } finally {
        fn.value.dispose();
      }
      return result;
    }
    if (this.objects.size >= 10000)
      throw new ProtocolError("Remote object limit reached");
    const id = this.id("object");
    this.objects.set(id, { context, handle: h.dup(), group });
    return {
      ...result,
      objectId: id,
      description: type === "function" ? "Function" : "Object",
    };
  }
  async result(context, r, params) {
    const vm = context.page.vm;
    if (r.error) {
      try {
        const e = vm.dump(r.error);
        return {
          result: {
            type: "object",
            subtype: "error",
            description: String(e?.message || "JavaScript exception"),
          },
          exceptionDetails: {
            exceptionId: this.next++,
            text: "Uncaught",
            lineNumber: 0,
            columnNumber: 0,
            executionContextId: context.id,
            exception: {
              type: "object",
              subtype: "error",
              description: JSON.stringify(e),
            },
          },
        };
      } finally {
        r.error.dispose();
      }
    }
    let h = r.value;
    try {
      if (params.awaitPromise) {
        const end = Date.now() + this.evaluationTimeoutMs;
        while (true) {
          const state = vm.getPromiseState(h);
          if (state.type === "fulfilled") {
            // A non-promise state borrows h; only the outer finally owns it.
            if (state.notAPromise) break;
            h.dispose();
            h = state.value;
            break;
          }
          if (state.type === "rejected") {
            h.dispose();
            h = null;
            return this.result(
              context,
              { error: state.error },
              { ...params, awaitPromise: false },
            );
          }
          if (Date.now() >= end)
            throw new ProtocolError("Promise evaluation deadline exceeded");
          context.page.pump();
          await new Promise((r) => setTimeout(r, 1));
        }
      }
      return {
        result: this.remote(
          context,
          h,
          params.returnByValue === true,
          params.objectGroup,
        ),
      };
    } finally {
      if (h?.alive) h.dispose();
    }
  }
  arg(c, arg) {
    const vm = c.page.vm;
    if (arg.objectId) {
      const o = this.objects.get(arg.objectId);
      if (!o || o.context !== c)
        throw new ProtocolError("Object belongs to another execution context");
      return o.handle.dup();
    }
    const code =
      arg.unserializableValue ??
      (arg.value === undefined ? "undefined" : JSON.stringify(arg.value));
    if (
      arg.unserializableValue &&
      !/^(NaN|Infinity|-Infinity|-0|-?\d+n)$/.test(code)
    )
      throw new ProtocolError("Invalid unserializable argument", -32602);
    const r = vm.evalCode(`(${code})`);
    if (r.error) {
      r.error.dispose();
      throw new ProtocolError("Invalid argument");
    }
    return r.value;
  }
  async send(method, p = {}, sessionId) {
    if (this.closed) throw new ProtocolError("Browser closed");
    if (typeof method !== "string" || !p || typeof p !== "object")
      throw new ProtocolError("Invalid CDP request", -32600);
    if (method === "Browser.getVersion")
      return {
        protocolVersion: "1.3",
        product: "Obscura/Wasm-experimental",
        revision: "5ba6c05",
        userAgent: "Obscura/Wasm",
        jsVersion: "QuickJS-ng",
      };
    if (method === "Target.getTargets")
      return {
        targetInfos: [...this.targets.values()].map((t) => this.targetInfo(t)),
      };
    if (method === "Target.createTarget")
      return { targetId: (await this.createTarget(p.url || "about:blank")).id };
    if (method === "Target.attachToTarget") {
      const t = this.targets.get(p.targetId);
      if (!t) throw new ProtocolError("Unknown target");
      const id = this.id("session");
      this.sessions.set(id, { target: t, runtime: false, page: false });
      return { sessionId: id };
    }
    if (method === "Target.detachFromTarget") {
      if (!this.sessions.delete(p.sessionId))
        throw new ProtocolError("Unknown session");
      return {};
    }
    if (method === "Target.getTargetInfo")
      return {
        targetInfo: this.targetInfo(
          this.targets.get(p.targetId) || this.targetFor(sessionId),
        ),
      };
    if (method === "Target.closeTarget") {
      const t = this.targets.get(p.targetId);
      if (!t) throw new ProtocolError("Unknown target");
      this.releaseTarget(t);
      this.targets.delete(t.id);
      this.onCloseTarget(t.id);
      for (const [id, s] of this.sessions)
        if (s.target === t) this.sessions.delete(id);
      return { success: true };
    }
    if (method === "Browser.close") {
      this.close();
      return {};
    }
    const t = this.targetFor(sessionId);
    this.syncContexts(t);
    if (method === "Page.enable" || method === "Page.disable") {
      this.sessions.get(sessionId).page = method.endsWith("enable");
      return {};
    }
    if (method === "Runtime.enable") {
      this.sessions.get(sessionId).runtime = true;
      for (const [id, c] of this.contexts)
        if (c.target === t) this.announce(id, sessionId);
      return {};
    }
    if (method === "Runtime.disable") {
      this.sessions.get(sessionId).runtime = false;
      return {};
    }
    if (method === "DOM.enable" || method === "DOM.disable") return {};
    if (method === "Page.navigate") return this.navigate(t, p.url);
    if (method === "Page.reload") return this.navigate(t, t.url);
    if (method === "Page.getFrameTree") {
      const tree = (page) => ({
        frame: {
          id: this.frameId(t, page),
          ...(page.frameId
            ? {
                parentId: this.frameId(
                  t,
                  t.host.pages.get(page.parentFrameId) || t.page,
                ),
              }
            : {}),
          loaderId: `${t.id}:${t.generation}`,
          url: page.run("location.href"),
          domainAndRegistry: "",
          securityOrigin: new URL(page.run("location.href")).origin,
          mimeType: "text/html",
        },
        childFrames: [...t.host.pages.values()]
          .filter(
            (c) =>
              c.frameId !== page.frameId &&
              (c.parentFrameId ?? 0) === page.frameId,
          )
          .map(tree),
      });
      return { frameTree: tree(t.page) };
    }
    if (method === "Runtime.evaluate") {
      if (p.throwOnSideEffect || p.replMode || p.serializationOptions)
        throw new ProtocolError("Requested evaluation option is unsupported");
      const c = this.context(t, p.contextId);
      if (!c) throw new ProtocolError("No execution context");
      c.page.run("void 0");
      return this.result(
        c,
        c.page.vm.evalCode(p.expression, "cdp-evaluate.js"),
        p,
      );
    }
    if (method === "Runtime.callFunctionOn") {
      const o = p.objectId ? this.objects.get(p.objectId) : undefined,
        c = o?.context || this.context(t, p.executionContextId);
      if (!c || c.target !== t || (p.objectId && !o))
        throw new ProtocolError("Unknown object or execution context");
      if (p.throwOnSideEffect || p.serializationOptions)
        throw new ProtocolError("Requested call option unsupported");
      const vm = c.page.vm;
      c.page.run("void 0");
      const fn = vm.evalCode(`(${p.functionDeclaration})`, "cdp-function.js");
      if (fn.error) return this.result(c, fn, p);
      const args = [];
      try {
        for (const a of p.arguments || []) args.push(this.arg(c, a));
        return await this.result(
          c,
          vm.callFunction(fn.value, o?.handle || vm.global, ...args),
          p,
        );
      } finally {
        fn.value.dispose();
        for (const a of args) a.dispose();
      }
    }
    if (method === "Runtime.releaseObject") {
      const o = this.objects.get(p.objectId);
      if (o && o.context.target === t) {
        o.handle.dispose();
        this.objects.delete(p.objectId);
      }
      return {};
    }
    if (method === "Runtime.releaseObjectGroup") {
      for (const [id, o] of this.objects)
        if (o.context.target === t && o.group === p.objectGroup) {
          o.handle.dispose();
          this.objects.delete(id);
        }
      return {};
    }
    if (method === "DOM.getDocument")
      return {
        root: this.describeNode(
          t,
          t.page,
          Number(t.page.dom.command("document_node_id", "", "")),
          p.depth ?? 1,
          p.pierce === true,
        ),
      };
    if (method === "DOM.describeNode") {
      const n = this.node(t, p.nodeId ?? p.backendNodeId);
      return {
        node: this.describeNode(
          t,
          n.page,
          n.nid,
          p.depth ?? 0,
          p.pierce === true,
        ),
      };
    }
    if (method === "DOM.querySelector" || method === "DOM.querySelectorAll") {
      const n = this.node(t, p.nodeId),
        many = method.endsWith("All"),
        raw = JSON.parse(
          n.page.dom.command(
            many ? "query_selector_all_scoped" : "query_selector_scoped",
            String(n.nid),
            p.selector,
          ),
        );
      return many
        ? { nodeIds: raw.map((id) => this.nodeId(t, n.page, id)) }
        : { nodeId: raw < 0 ? 0 : this.nodeId(t, n.page, raw) };
    }
    if (method === "DOM.getOuterHTML") {
      const n = this.node(t, p.nodeId ?? p.backendNodeId);
      return {
        outerHTML: JSON.parse(
          n.page.dom.command("outer_html", String(n.nid), ""),
        ),
      };
    }
    if (method === "DOM.getAttributes") {
      const n = this.node(t, p.nodeId);
      return {
        attributes: JSON.parse(
          n.page.dom.command("attribute_names", String(n.nid), ""),
        ).flatMap((k) => [
          k,
          JSON.parse(n.page.dom.command("get_attribute", String(n.nid), k)),
        ]),
      };
    }
    if (method === "DOM.resolveNode") {
      const n = this.node(t, p.nodeId ?? p.backendNodeId),
        c = this.context(
          t,
          p.executionContextId ??
            [...this.contexts].find(([, c]) => c.page === n.page)?.[0],
        );
      if (c.page !== n.page)
        throw new ProtocolError(
          "Node and execution context belong to different frames",
        );
      c.page.run("void 0");
      const r = c.page.vm.evalCode(this.nodeExpr(n));
      if (r.error) {
        r.error.dispose();
        throw new ProtocolError("Cannot resolve node");
      }
      try {
        return { object: this.remote(c, r.value, false, p.objectGroup) };
      } finally {
        r.value.dispose();
      }
    }
    if (method === "DOM.focus") {
      const n = this.node(t, p.nodeId ?? p.backendNodeId);
      n.page.run(`${this.nodeExpr(n)}.focus()`);
      t.focused = n.page;
      return {};
    }
    if (method === "Input.insertText") {
      const page = t.focused || t.page;
      page.run(
        `(()=>{const el=document.activeElement;if(!el||!('value' in el))throw new Error('No editable element focused');if(el.disabled||el.readOnly)throw new Error('Focused element is not editable');const text=${JSON.stringify(p.text)};const before=new InputEvent('beforeinput',{bubbles:true,cancelable:true,data:text,inputType:'insertText'});if(!el.dispatchEvent(__obscura_markTrusted(before)))return;let proto=Object.getPrototypeOf(el),setter;while(proto&&!(setter=Object.getOwnPropertyDescriptor(proto,'value')?.set))proto=Object.getPrototypeOf(proto);if(!setter)throw new Error('No native editable value setter');setter.call(el,el.value+text);el.dispatchEvent(__obscura_markTrusted(new InputEvent('input',{bubbles:true,data:text,inputType:'insertText'})));})()`,
      );
      return {};
    }
    throw new ProtocolError(`Obscura does not implement ${method}`, -32601);
  }
  close() {
    if (this.closed) return;
    for (const t of this.targets.values()) this.releaseTarget(t);
    this.targets.clear();
    this.sessions.clear();
    this.closed = true;
  }
}
