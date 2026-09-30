/** Experimental browser host: original Obscura bootstrap + genuine Obscura Wasm DOM + QuickJS. */
export function createObscuraHost({
  quickJs,
  WasmDom,
  bootstrap,
  fetch: hostFetch,
  crypto: hostCrypto = globalThis.crypto,
  URL: NativeURL = globalThis.URL,
  TextDecoder: NativeTextDecoder = globalThis.TextDecoder,
  setTimeout: hostSetTimeout = globalThis.setTimeout,
  clearTimeout: hostClearTimeout = globalThis.clearTimeout,
  maxInterruptCallbacks = 10000,
  maxJobsPerPump = 1000,
  evaluationTimeoutMs = 10000,
  maxModulePrefetches = 256,
  maxFrames = 16,
  maxDiagnosticEntries = 256,
  session,
  tabId = "default",
  installPageGlobals,
}) {
  if (!Number.isInteger(maxFrames) || maxFrames < 1)
    throw new RangeError("maxFrames must be a positive integer");
  if (!Number.isInteger(maxDiagnosticEntries) || maxDiagnosticEntries < 0)
    throw new RangeError("maxDiagnosticEntries must be a nonnegative integer");
  const pages = new Map(),
    errors = [],
    logs = [],
    tasks = new Set(),
    parserTasks = new Set(),
    pendingFrames = new Set(),
    creatingFrames = new Set();
  let nextFrame = 1;
  const append = (array, value) => {
    if (!maxDiagnosticEntries) return;
    array.push(value);
    if (array.length > maxDiagnosticEntries)
      array.splice(0, array.length - maxDiagnosticEntries);
  };
  const assertFrameCapacity = () => {
    if (pages.size + pendingFrames.size + creatingFrames.size >= maxFrames)
      throw new Error("Maximum frame count exceeded: " + maxFrames);
  };
  function createPage({ html, url, frameId = 0, parentFrameId = 0 }) {
    if (
      pages.has(frameId) ||
      pendingFrames.has(frameId) ||
      creatingFrames.has(frameId)
    )
      throw new Error("Frame ID already in use: " + frameId);
    assertFrameCapacity();
    creatingFrames.add(frameId);
    nextFrame = Math.max(nextFrame, frameId + 1);
    let dom, vm, cleanup, pageGlobalsCleanup;
    const fetchResource = (target, init, kind) =>
      session
        ? session.fetch(target, init, { documentURL: url, kind, tabId })
        : hostFetch(target, init);
    try {
      dom = new WasmDom(html, url);
      vm = quickJs.newContext();
      const rt = vm.runtime;
      rt.setMemoryLimit(64 * 1024 * 1024);
      rt.setMaxStackSize(512 * 1024);
      let closed = false,
        nextTimer = 1,
        deadline = Date.now() + evaluationTimeoutMs,
        interrupts = 0;
      rt.setInterruptHandler(
        () =>
          closed ||
          Date.now() > deadline ||
          ++interrupts > maxInterruptCallbacks,
      );
      const timers = new Map(),
        pending = new Set(),
        pageTasks = new Map(),
        opCalls = [],
        moduleSources = new Map(),
        moduleFetches = new Map(),
        moduleWaits = new Set();
      let moduleSequence = 0;
      const externalStylesheets = new Map(),
        cssomStylesheets = new Map();
      let stylesheetGeneration = 0;
      const checkStyleFrame = (requestedFrame) => {
        if (requestedFrame !== frameId)
          throw new Error("Incorrect stylesheet frame");
      };
      const saveStylesheet = (map, id, value) => {
        const candidate = new Map(map).set(id, value);
        const external =
          map === externalStylesheets ? candidate : externalStylesheets;
        const cssom = map === cssomStylesheets ? candidate : cssomStylesheets;
        const size =
          [...external.values()].reduce(
            (n, sheet) => n + 2 * sheet.css.length,
            0,
          ) +
          [...cssom.values()].reduce(
            (n, rules) => n + 2 * rules.join("").length,
            0,
          );
        if (external.size + cssom.size > 512 || size > 16 * 1024 * 1024)
          throw new Error("Stylesheet storage budget exceeded");
        map.set(id, value);
        stylesheetGeneration++;
      };
      const take = (result) => {
        if (result.error) {
          const e = vm.dump(result.error);
          result.error.dispose();
          throw new Error(JSON.stringify(e));
        }
        return result.value;
      };
      const run = (source, name = "page.js") => {
        if (closed) throw new Error("Page closed");
        deadline = Date.now() + evaluationTimeoutMs;
        interrupts = 0;
        const h = take(vm.evalCode(source, name));
        try {
          return vm.dump(h);
        } finally {
          if (h.alive) h.dispose();
        }
      };
      const pump = () => {
        if (closed) return;
        deadline = Date.now() + evaluationTimeoutMs;
        interrupts = 0;
        const r = rt.executePendingJobs(maxJobsPerPump);
        if (r.error) {
          const e = vm.dump(r.error);
          r.error.dispose();
          append(errors, { frameId, phase: "jobs", error: e });
        }
      };
      const expose = (name, fn) => {
        const h = vm.newFunction(name, fn);
        vm.setProp(vm.global, name, h);
        h.dispose();
      };
      const json = (value) => vm.newString(JSON.stringify(value));
      const wireOps = new Set();
      // QuickJS's C-string dump truncates embedded NULs. Serialize complete
      // argument tuples in the guest before crossing FFI (DOM attributes use
      // NUL delimiters). Callback-bearing ops keep their handle-based bridge.
      const sync = (name, fn) => {
        wireOps.add(name);
        expose(name, (encoded) => {
          const values = JSON.parse(vm.getString(encoded));
          append(opCalls, [name, ...values]);
          const v = fn(...values);
          if (v === undefined) return vm.undefined;
          if (v === null) return vm.null;
          if (typeof v === "string") return vm.newString(v);
          if (typeof v === "number") return vm.newNumber(v);
          if (typeof v === "boolean") return v ? vm.true : vm.false;
          return json(v);
        });
      };
      const cancelTask = (timer) => {
        const release = pageTasks.get(timer);
        if (!release) return;
        hostClearTimeout(timer);
        tasks.delete(timer);
        pageTasks.delete(timer);
        release();
      };
      const later = (fn, delay = 0, release = () => {}) => {
        const timer = hostSetTimeout(() => {
          tasks.delete(timer);
          pageTasks.delete(timer);
          try {
            if (!closed) {
              deadline = Date.now() + evaluationTimeoutMs;
              interrupts = 0;
              try {
                fn();
                pump();
              } catch (e) {
                append(errors, { frameId, phase: "task", error: String(e) });
              }
            }
          } finally {
            release();
          }
        }, delay);
        tasks.add(timer);
        pageTasks.set(timer, release);
        return timer;
      };
      cleanup = () => {
        if (closed) return;
        closed = true;
        for (const child of [...pages.values()])
          if (child.frameId !== frameId && child.parentFrameId === frameId)
            child.close();
        for (const timer of [...pageTasks.keys()]) cancelTask(timer);
        timers.clear();
        for (const finish of [...moduleWaits]) finish(new Error("Page closed"));
        for (const p of pending) p.dispose();
        pending.clear();
        pages.delete(frameId);
        pageGlobalsCleanup?.();
        try {
          vm.dispose();
        } finally {
          dom.free?.();
        }
      };
      const asyncOp = (name, fn) => {
        wireOps.add(name);
        expose(name, (encoded) => {
          const values = JSON.parse(vm.getString(encoded)),
            d = vm.newPromise();
          pending.add(d);
          Promise.resolve()
            .then(() => fn(...values))
            .then(
              (value) => {
                if (!closed) {
                  const h = vm.newString(
                    typeof value === "string" ? value : JSON.stringify(value),
                  );
                  d.resolve(h);
                  h.dispose();
                }
              },
              (error) => {
                if (!closed) {
                  const h = vm.newError(String(error));
                  d.reject(h);
                  h.dispose();
                }
              },
            )
            .finally(() => {
              if (!closed) {
                pending.delete(d);
                pump();
                d.dispose();
              }
            });
          return d.handle;
        });
      };
      sync("op_dom", (cmd, a1, a2, requestedFrame) => {
        if (requestedFrame !== frameId) throw new Error("Incorrect DOM frame");
        if (
          /^(set_|append_child|remove_child|insert_before|document_write)/.test(
            cmd,
          )
        )
          stylesheetGeneration++;
        return dom.command(cmd, a1, a2);
      });
      sync("op_session_history", () => "0,0,1");
      sync("op_async_runtime_available", () => true);
      sync("op_script_try_start", (nid) => dom.script_try_start(nid));
      sync("op_script_mark_started", (nid) => dom.script_mark_started(nid));
      sync("op_runtime_events_enabled", () => false);
      sync("op_console_msg", (level, text) =>
        append(logs, { frameId, level, text }),
      );
      sync("op_begin_render_task", () => {});
      sync("op_posted_task_generation", () => 1);
      const components = (u) =>
        JSON.stringify({
          ok: true,
          ...Object.fromEntries(
            [
              "href",
              "protocol",
              "username",
              "password",
              "host",
              "hostname",
              "port",
              "pathname",
              "search",
              "hash",
              "origin",
            ].map((k) => [k, u[k]]),
          ),
        });
      sync("op_url_parse", (s, base) => {
        try {
          return components(base ? new NativeURL(s, base) : new NativeURL(s));
        } catch {
          return "null";
        }
      });
      sync("op_url_resolve", (s, base) => {
        try {
          return (base ? new NativeURL(s, base) : new NativeURL(s)).href;
        } catch {
          return "";
        }
      });
      sync("op_url_set", (s, part, value) => {
        try {
          const u = new NativeURL(s);
          u[part] = value;
          return components(u);
        } catch {
          return "null";
        }
      });
      sync("op_encoding_for_label", (label) => {
        try {
          return new NativeTextDecoder(label).encoding;
        } catch {
          return "";
        }
      });
      sync("op_text_decode", (encoding, bytes, fatal, ignoreBOM) => {
        try {
          return JSON.stringify({
            ok: true,
            v: new NativeTextDecoder(encoding, { fatal, ignoreBOM }).decode(
              new Uint8Array(Object.values(bytes || {})),
            ),
          });
        } catch {
          return JSON.stringify({ ok: false });
        }
      });
      expose("op_random_bytes", (length) => {
        const size = vm.getNumber(length);
        if (!Number.isInteger(size) || size < 0 || size > 65536)
          throw new RangeError(
            "Random byte count must be an integer in [0, 65536]",
          );
        if (!hostCrypto?.getRandomValues)
          throw new Error("Native cryptographic randomness is unavailable");
        const bytes = hostCrypto.getRandomValues(new Uint8Array(size)),
          array = vm.newArray();
        try {
          for (let i = 0; i < bytes.length; i++) {
            const value = vm.newNumber(bytes[i]);
            try {
              vm.setProp(array, i, value);
            } finally {
              value.dispose();
            }
          }
          return array;
        } catch (error) {
          array.dispose();
          throw error;
        }
      });
      sync("op_get_cookies", () => session?.getDocumentCookie(url) || "");
      sync("op_set_cookie", (value) => {
        if (session) return session.setDocumentCookie(url, value);
        throw new Error("Cookie persistence not implemented");
      });
      // Retain real fetched CSS and CSSOM edits for the bootstrap's stylesheet
      // model. This enables load/rule inspection; there is still no renderer.
      sync("op_stylesheet_generation", (frame) => {
        checkStyleFrame(frame);
        return stylesheetGeneration;
      });
      sync(
        "op_external_stylesheet_set",
        (id, css, href, originClean, frame) => {
          checkStyleFrame(frame);
          saveStylesheet(externalStylesheets, id, {
            css: String(css),
            href: String(href),
            originClean: originClean === true,
          });
        },
      );
      sync("op_external_stylesheet_get", (id, frame) => {
        checkStyleFrame(frame);
        return JSON.stringify(externalStylesheets.get(id) || null);
      });
      sync("op_external_stylesheet_remove", (id, frame) => {
        checkStyleFrame(frame);
        externalStylesheets.delete(id);
        stylesheetGeneration++;
      });
      sync("op_cssom_stylesheet_has", (id, frame) => {
        checkStyleFrame(frame);
        return cssomStylesheets.has(id);
      });
      sync("op_cssom_stylesheet_clear", (id, frame) => {
        checkStyleFrame(frame);
        cssomStylesheets.delete(id);
        stylesheetGeneration++;
      });
      sync(
        "op_cssom_stylesheet_update",
        (id, index, count, rules, reset, frame) => {
          checkStyleFrame(frame);
          const next = reset ? [] : [...(cssomStylesheets.get(id) || [])];
          next.splice(index, count, ...rules.map(String));
          saveStylesheet(cssomStylesheets, id, next);
          return true;
        },
      );
      sync("op_shadow_root_info", (nid) => dom.shadow_root_info(nid));
      sync("op_shadow_attach", (nid, mode) => dom.shadow_attach(nid, mode));
      asyncOp(
        "op_fetch_url",
        async (
          target,
          method,
          headers,
          body,
          _origin,
          _mode,
          _credentials,
          _script,
          redirect,
        ) => {
          const bytes = Array.isArray(body) ? body : Object.values(body || {});
          const response = await fetchResource(
            target,
            {
              method,
              mode: _mode,
              redirect,
              credentials: _credentials,
              headers: JSON.parse(headers),
              ...(bytes.length ? { body: new Uint8Array(bytes) } : {}),
            },
            _script === "navigation"
              ? "navigation"
              : _script
                ? "script"
                : "fetch",
          );
          return {
            status: response.status,
            type: response.type,
            body: await response.text(),
            headers: Object.fromEntries(response.headers),
            url: response.url || target,
            redirected: response.redirected,
          };
        },
      );
      expose("op_sleep", (delay) => {
        const d = vm.newPromise();
        pending.add(d);
        later(() => {
          pending.delete(d);
          d.resolve();
          pump();
          d.dispose();
        }, vm.getNumber(delay));
        return d.handle;
      });
      expose("__hostCreateTimer", (callback, delay) => {
        const id = nextTimer++,
          fn = callback.dup();
        const native = later(
          () => {
            timers.delete(id);
            take(vm.callFunction(fn, vm.undefined)).dispose();
          },
          vm.getNumber(delay),
          () => fn.dispose(),
        );
        timers.set(id, { native });
        return vm.newNumber(id);
      });
      sync("__hostCancelTimer", (id) => {
        const t = timers.get(id);
        if (t) {
          cancelTask(t.native);
          timers.delete(id);
        }
      });
      expose("op_posted_task", (_frame, callback) => {
        const fn = callback.dup();
        later(
          () => {
            const generation = vm.newNumber(1);
            try {
              take(vm.callFunction(fn, vm.undefined, generation)).dispose();
            } finally {
              generation.dispose();
            }
          },
          0,
          () => fn.dispose(),
        );
        return vm.newNumber(1);
      });
      sync("op_frame_document_ready", (childUrl, childHtml) => {
        assertFrameCapacity();
        const id = nextFrame++;
        pendingFrames.add(id);
        try {
          later(
            () => {
              pendingFrames.delete(id);
              const child = createPage({
                html: childHtml,
                url: childUrl,
                frameId: id,
                parentFrameId: frameId,
              });
              const loading = child.runParserScriptsAsync();
              parserTasks.add(loading);
              loading
                .catch((error) =>
                  append(errors, {
                    frameId: id,
                    phase: "frame-parser",
                    error: String(error),
                  }),
                )
                .finally(() => {
                  parserTasks.delete(loading);
                  // The iframe load event follows the child parser scripts;
                  // parents commonly send initialization data from onload.
                  if (!closed && pages.has(id)) {
                    run(
                      `(()=>{const el=globalThis.__obscura_frameElements[${id}];if(el?._frameId===${id})el.dispatchEvent(new Event('load'));})()`,
                      "frame-load.js",
                    );
                    pump();
                  }
                });
            },
            0,
            () => pendingFrames.delete(id),
          );
        } catch (error) {
          pendingFrames.delete(id);
          throw error;
        }
        return id;
      });
      sync(
        "op_post_frame_message",
        (
          targetFrameId,
          _claimedSource,
          _claimedOrigin,
          targetOrigin,
          dataJson,
        ) => {
          // Sender identity comes from this host closure, never guest-supplied IDs.
          const origin = new NativeURL(url).origin;
          later(() => {
            const target = pages.get(targetFrameId);
            if (!target) return;
            const expected =
              !targetOrigin || targetOrigin === "/"
                ? origin
                : targetOrigin === "*"
                  ? "*"
                  : new NativeURL(targetOrigin).origin;
            if (
              expected !== "*" &&
              new NativeURL(target.url).origin !== expected
            )
              return;
            target.run(
              `__obscura_deliverMessage(${JSON.stringify(dataJson)},${JSON.stringify(origin)},${frameId},${JSON.stringify(targetOrigin || "/")});`,
              "frame-message.js",
            );
            target.pump();
          });
        },
      );
      const opNames = [
        "op_random_bytes",
        "op_dom",
        "op_session_history",
        "op_async_runtime_available",
        "op_script_try_start",
        "op_script_mark_started",
        "op_runtime_events_enabled",
        "op_console_msg",
        "op_begin_render_task",
        "op_posted_task_generation",
        "op_url_parse",
        "op_url_resolve",
        "op_url_set",
        "op_encoding_for_label",
        "op_text_decode",
        "op_get_cookies",
        "op_set_cookie",
        "op_external_stylesheet_set",
        "op_external_stylesheet_get",
        "op_external_stylesheet_remove",
        "op_cssom_stylesheet_update",
        "op_stylesheet_generation",
        "op_cssom_stylesheet_has",
        "op_cssom_stylesheet_clear",
        "op_shadow_root_info",
        "op_shadow_attach",
        "op_fetch_url",
        "op_sleep",
        "op_posted_task",
        "op_frame_document_ready",
        "op_post_frame_message",
      ];
      run(
        `(()=>{const encode=JSON.stringify;globalThis.Deno={core:{ops:{${opNames.map((n) => `${n}:${wireOps.has(n) ? `((native)=>(...args)=>native(encode(args)))(${n})` : n}`).join(",")}},createTimer:__hostCreateTimer,cancelTimer:((native)=>(...args)=>native(encode(args)))(__hostCancelTimer),setUnhandledPromiseRejectionHandler(){},setHandledPromiseRejectionHandler(){}}};})();`,
        "host-setup.js",
      );
      run(bootstrap, "bootstrap.js");
      run(
        `globalThis.__obscura_frameId=${frameId};globalThis.__obscura_parentFrameId=${parentFrameId};__obscura_init();delete globalThis.Deno;delete globalThis.__obscura_core_handoff;${opNames.map((n) => `delete globalThis.${n};`).join("")}delete globalThis.__hostCreateTimer;delete globalThis.__hostCancelTimer;`,
        "host-init.js",
      );
      pageGlobalsCleanup = installPageGlobals?.({
        vm,
        run,
        frameId,
        url,
        tabId,
      });
      // QuickJS's synchronous loader must never return a host Promise. Discover
      // static dependencies using compileOnly in disposable, isolated runtimes.
      // Failed linking cannot poison the live realm's module cache; no probe ever
      // executes module bodies. This also handles cycles, re-exports and syntax
      // containing misleading import-like strings without a source-code regex.
      const normalizeModule = (base, name) => {
        if (!/^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|\/|\.\.?\/)/.test(name))
          throw new Error(
            "Bare module specifier requires an import map: " + name,
          );
        return new NativeURL(name, base).href;
      };
      const cachedModule = (name) => {
        if (!moduleSources.has(name))
          throw new Error(
            "Module not prefetched (uncached dynamic import is unsupported): " +
              name,
          );
        return moduleSources.get(name);
      };
      rt.setModuleLoader(cachedModule, normalizeModule);
      const fetchModule = async (target) => {
        if (moduleSources.has(target)) return;
        if (!moduleFetches.has(target))
          moduleFetches.set(
            target,
            (async () => {
              const response = await fetchResource(
                target,
                { method: "GET", mode: "cors", credentials: "same-origin" },
                "module",
              );
              if (!response.ok)
                throw new Error("HTTP " + response.status + " " + target);
              const source = await response.text();
              if (closed) throw new Error("Page closed");
              // Redirect base-URL remapping and browser module MIME/CORS policy still
              // require a full resource loader; fail explicitly on changed module URLs.
              if (response.url && response.url !== target)
                throw new Error(
                  "Redirected module URLs are not supported: " +
                    target +
                    " -> " +
                    response.url,
                );
              moduleSources.set(target, source);
            })(),
          );
        return moduleFetches.get(target);
      };
      const prefetchModule = async (target) => {
        await fetchModule(target);
        for (let attempt = 0; attempt <= maxModulePrefetches; attempt++) {
          if (closed) throw new Error("Page closed");
          let missing, compileError;
          const probe = quickJs.newContext(),
            probeRuntime = probe.runtime;
          try {
            let probeInterrupts = 0;
            const probeDeadline = Date.now() + evaluationTimeoutMs;
            probeRuntime.setMemoryLimit(64 * 1024 * 1024);
            probeRuntime.setMaxStackSize(512 * 1024);
            probeRuntime.setInterruptHandler(
              () =>
                closed ||
                Date.now() > probeDeadline ||
                ++probeInterrupts > maxInterruptCallbacks,
            );
            probeRuntime.setModuleLoader((name) => {
              if (!moduleSources.has(name)) {
                missing = name;
                throw new Error("Module prefetch required: " + name);
              }
              return moduleSources.get(name);
            }, normalizeModule);
            const result = probe.evalCode(
              "import " + JSON.stringify(target) + ";",
              target + "#obscura-probe",
              { type: "module", compileOnly: true },
            );
            if (result.error) {
              compileError = probe.dump(result.error);
              result.error.dispose();
            } else result.value.dispose();
          } finally {
            probe.dispose();
          }
          if (missing) {
            if (attempt === maxModulePrefetches)
              throw new Error("Module graph exceeds prefetch budget");
            await fetchModule(missing);
            continue;
          }
          if (compileError) throw new Error(JSON.stringify(compileError));
          return target;
        }
      };
      const awaitModule = (handle) =>
        new Promise((resolve, reject) => {
          const started = Date.now();
          let polls = 0,
            finished = false,
            timer;
          const finish = (error) => {
            if (finished) return;
            finished = true;
            if (timer !== undefined) cancelTask(timer);
            moduleWaits.delete(finish);
            if (handle.alive) handle.dispose();
            error ? reject(error) : resolve();
          };
          moduleWaits.add(finish);
          const check = () => {
            try {
              if (closed) return finish(new Error("Page closed"));
              pump();
              const state = vm.getPromiseState(handle);
              if (state.type === "fulfilled") {
                state.value.dispose();
                return finish();
              }
              if (state.type === "rejected") {
                const error = vm.dump(state.error);
                state.error.dispose();
                return finish(new Error(JSON.stringify(error)));
              }
              if (
                Date.now() - started > evaluationTimeoutMs ||
                ++polls > evaluationTimeoutMs
              )
                return finish(
                  new Error("Module evaluation did not settle before deadline"),
                );
              timer = later(check, 1);
            } catch (error) {
              finish(error);
            }
          };
          check();
        });
      const runModule = async (source, moduleURL) => {
        const target = new NativeURL(
          moduleURL || "#obscura-inline-" + ++moduleSequence,
          url,
        ).href;
        if (source !== undefined && !moduleSources.has(target))
          moduleSources.set(target, source);
        await prefetchModule(target);
        if (closed) throw new Error("Page closed");
        deadline = Date.now() + evaluationTimeoutMs;
        interrupts = 0;
        // An import entry point lets QuickJS own singleton evaluation and live
        // bindings even when multiple script elements reference the same module.
        const handle = take(
          vm.evalCode(
            "import " + JSON.stringify(target) + ";",
            url + "#obscura-entry-" + ++moduleSequence,
            { type: "module" },
          ),
        );
        await awaitModule(handle);
      };
      const page = {
        frameId,
        parentFrameId,
        url,
        dom,
        vm,
        run,
        pump,
        opCalls,
        pending,
        timers,
        runModule,
        prefetchModule,
        runParserScripts() {
          const list = run(
            `Array.from(document.querySelectorAll('script')).map(s=>({nid:s._nid,src:s.getAttribute('src'),type:s.getAttribute('type'),text:s.textContent}))`,
          );
          for (const s of list) {
            dom.script_mark_started(s.nid);
            if (s.src || s.type === "module")
              throw new Error(
                "Parser external/module scripts not yet supported by this probe",
              );
            run(`globalThis.__currentScriptNid=${s.nid};`);
            try {
              run(s.text, "parser-script.js");
            } finally {
              run("globalThis.__currentScriptNid=0;");
            }
          }
          pump();
        },
        async runParserScriptsAsync() {
          const list = run(
            `Array.from(document.querySelectorAll('script')).map(s=>({nid:s._nid,src:s.getAttribute('src'),type:(s.getAttribute('type')||'').toLowerCase(),text:s.textContent,async:s.hasAttribute('async'),defer:s.hasAttribute('defer')}))`,
          );
          const deferred = [],
            asynchronous = [],
            results = [];
          page.parserResults = results;
          const execute = async (s) => {
            if (closed) return;
            try {
              let source = s.text;
              if (s.type === "module") {
                await runModule(
                  s.src ? undefined : source,
                  s.src
                    ? new NativeURL(s.src, run("document.baseURI")).href
                    : new NativeURL(
                        "#obscura-inline-script-" + s.nid,
                        run("document.baseURI"),
                      ).href,
                );
                results.push({
                  nid: s.nid,
                  src: s.src,
                  status: "executed",
                  type: "module",
                });
                if (s.src && !closed)
                  run(
                    `document.querySelectorAll('script').forEach(s=>{if(s._nid===${s.nid})s.dispatchEvent(new Event('load'));});`,
                  );
                return;
              }
              if (s.src) {
                const target = new NativeURL(s.src, run("document.baseURI"))
                  .href;
                const response = await fetchResource(
                  target,
                  { method: "GET" },
                  "script",
                );
                if (!response.ok)
                  throw new Error("HTTP " + response.status + " " + target);
                source = await response.text();
              }
              if (closed) return;
              run(`globalThis.__currentScriptNid=${s.nid};`);
              try {
                run(source, s.src || "parser-inline.js");
                results.push({ nid: s.nid, src: s.src, status: "executed" });
              } finally {
                run("globalThis.__currentScriptNid=0;");
              }
              if (s.src)
                run(
                  `document.querySelectorAll('script').forEach(s=>{if(s._nid===${s.nid})s.dispatchEvent(new Event('load'));});`,
                );
              pump();
            } catch (e) {
              results.push({
                nid: s.nid,
                src: s.src,
                status: "error",
                error: String(e),
              });
              append(errors, {
                frameId,
                phase: "parser",
                src: s.src,
                error: String(e),
              });
              if (s.src && !closed)
                run(
                  `document.querySelectorAll('script').forEach(s=>{if(s._nid===${s.nid})s.dispatchEvent(new Event('error'));});`,
                );
            }
          };
          run("globalThis.__documentReadyState__='loading';");
          for (const s of list) {
            if (
              s.type &&
              !["module", "text/javascript", "application/javascript"].includes(
                s.type,
              )
            )
              continue;
            dom.script_mark_started(s.nid);
            if ((s.src || s.type === "module") && s.async)
              asynchronous.push(execute(s));
            else if (s.type === "module" || (s.src && s.defer))
              deferred.push(s);
            else await execute(s);
          }
          if (closed) return results;
          for (const s of deferred) await execute(s);
          if (closed) return results;
          run(
            "globalThis.__documentReadyState__='interactive';document.dispatchEvent(new Event('DOMContentLoaded',{bubbles:true}));",
          );
          pump();
          await Promise.all(asynchronous);
          if (closed) return results;
          run(
            "globalThis.__documentReadyState__='complete';globalThis.dispatchEvent(new Event('load'));",
          );
          pump();
          return results;
        },
        close() {
          cleanup();
        },
      };
      pages.set(frameId, page);
      return page;
    } catch (error) {
      if (cleanup) cleanup();
      else {
        try {
          vm?.dispose();
        } finally {
          dom?.free?.();
        }
      }
      throw error;
    } finally {
      creatingFrames.delete(frameId);
    }
  }
  return {
    createPage,
    pages,
    errors,
    logs,
    async settle(ms = 2000) {
      const until = Date.now() + ms;
      let idle = 0;
      do {
        await new Promise((r) => hostSetTimeout(r, 1));
        for (const p of pages.values()) p.pump();
        const active =
          tasks.size +
          parserTasks.size +
          [...pages.values()].reduce(
            (n, p) =>
              n + p.pending.size + (p.vm.runtime.hasPendingJob() ? 1 : 0),
            0,
          );
        idle = active ? 0 : idle + 1;
        if (idle >= 2) return;
      } while (Date.now() < until);
      throw new Error("Browser did not settle before deadline");
    },
    close() {
      for (const p of [...pages.values()]) p.close();
    },
  };
}
