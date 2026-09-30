import { checkCors, preflight, exposedCorsHeaders } from "./cors.mjs";
import { CookieJar, Cookie } from "tough-cookie";
import ipaddr from "ipaddr.js";
const forbidden = new Set([
  "authorization",
  "access-control-request-headers",
  "access-control-request-method",
  "proxy-authorization",
  "cookie",
  "cookie2",
  "host",
  "origin",
  "referer",
  "connection",
  "content-length",
  "transfer-encoding",
  "upgrade",
  "via",
  "set-cookie",
]);
const redirects = new Set([301, 302, 303, 307, 308]);
export function publicHttpURL(value, base) {
  const u = new URL(value, base);
  if (!["http:", "https:"].includes(u.protocol) || u.username || u.password)
    throw new TypeError("Only credential-free public HTTP(S) URLs are allowed");
  const h = u.hostname
    .toLowerCase()
    .replace(/\.$/, "")
    .replace(/^\[|\]$/g, "");
  if (ipaddr.isValid(h)) {
    if (ipaddr.parse(h).range() !== "unicast")
      throw new TypeError("Non-public IP address blocked");
  } else if (
    !h.includes(".") ||
    [
      "localhost",
      "local",
      "internal",
      "lan",
      "home",
      "home.arpa",
      "onion",
      "invalid",
    ].some((s) => h === s || h.endsWith("." + s)) ||
    h === "metadata.goog"
  )
    throw new TypeError("Internal hostname blocked");
  u.hash = "";
  return u;
}
const origin = (value) => {
  const u = new URL(value);
  if (!["http:", "https:"].includes(u.protocol))
    throw new TypeError("Storage requires an HTTP(S) origin");
  return u.origin;
};
const jar = () =>
  new CookieJar(undefined, {
    rejectPublicSuffixes: true,
    allowSpecialUseDomain: false,
    allowSecureOnLocal: false,
    prefixSecurity: "strict",
  });
/** Caller supplies a public-only, redirect-manual native transport. No ambient fetch. */
export class BrowserSession {
  #transport;
  #jar = jar();
  #local = new Map();
  #tabs = new Map();
  #allow;
  #quota;
  constructor({
    transport,
    allowCrossOriginScripts = false,
    storageQuotaBytes = 5 * 1024 * 1024,
  } = {}) {
    if (
      !transport ||
      typeof transport.fetch !== "function" ||
      transport.publicOnly !== true
    )
      throw new TypeError("Public-only host transport required");
    this.#transport = transport;
    this.#allow = allowCrossOriginScripts;
    this.#quota = storageQuotaBytes;
  }
  getDocumentCookie(url) {
    origin(url);
    return this.#jar.getCookieStringSync(url, { http: false });
  }
  setDocumentCookie(url, value) {
    origin(url);
    try {
      this.#setCookie(String(value), url, false);
    } catch {}
  }
  #setCookie(value, url, http) {
    const c = Cookie.parse(value);
    if (
      !c ||
      (c.sameSite === "none" && !c.secure) ||
      (c.secure && new URL(url).protocol !== "https:")
    )
      return;
    if (
      new URL(url).protocol === "http:" &&
      this.#jar
        .getCookiesSync(url.replace(/^http:/, "https:"), { http: true })
        .some((old) => old.secure && old.key === c.key)
    )
      return;
    this.#jar.setCookieSync(c, url, { http, ignoreError: false });
  }
  storage(url, tabId = "default", kind = "local") {
    const o = origin(url);
    let origins;
    if (kind === "local") origins = this.#local;
    else if (kind === "session") {
      tabId = String(tabId);
      if (!this.#tabs.has(tabId)) this.#tabs.set(tabId, new Map());
      origins = this.#tabs.get(tabId);
    } else throw new TypeError("Unknown storage kind");
    if (!origins.has(o)) origins.set(o, new Map());
    return origins.get(o);
  }
  storageOperation(url, tab, kind, op, key, value) {
    const data = this.storage(url, tab, kind);
    switch (op) {
      case "entries":
        return [...data];
      case "get":
        return data.get(String(key)) ?? null;
      case "set": {
        key = String(key);
        value = String(value);
        const next = new Map(data);
        next.set(key, value);
        if (
          [...next].reduce((n, [k, v]) => n + 2 * (k.length + v.length), 0) >
          this.#quota
        )
          throw new Error("QuotaExceededError");
        data.set(key, value);
        return null;
      }
      case "remove":
        data.delete(String(key));
        return null;
      case "clear":
        data.clear();
        return null;
      default:
        throw new TypeError("Unknown storage operation");
    }
  }
  closeTab(tab) {
    this.#tabs.delete(String(tab));
  }
  exportSnapshot() {
    return {
      version: 1,
      cookieJar: this.#jar.serializeSync(),
      localStorage: [...this.#local].map(([o, m]) => [o, [...m]]),
      sessionStorage: [...this.#tabs].map(([t, os]) => [
        t,
        [...os].map(([o, m]) => [o, [...m]]),
      ]),
    };
  }
  importSnapshot(s) {
    if (
      !s ||
      s.version !== 1 ||
      !Array.isArray(s.localStorage) ||
      !Array.isArray(s.sessionStorage)
    )
      throw new TypeError("Invalid browser snapshot");
    const parse = (entries) =>
      new Map(
        entries.map(([o, items]) => {
          if (origin(o) !== o || !Array.isArray(items))
            throw new TypeError("Invalid origin");
          const m = new Map(
            items.map(([k, v]) => {
              if (typeof k !== "string" || typeof v !== "string")
                throw new TypeError("Invalid storage");
              return [k, v];
            }),
          );
          if (
            [...m].reduce((n, [k, v]) => n + 2 * (k.length + v.length), 0) >
            this.#quota
          )
            throw new TypeError("Storage exceeds quota");
          return [o, m];
        }),
      );
    const local = parse(s.localStorage),
      tabs = new Map(
        s.sessionStorage.map(([t, o]) => {
          if (typeof t !== "string") throw new TypeError("Invalid tab");
          return [t, parse(o)];
        }),
      ),
      restored = CookieJar.deserializeSync(s.cookieJar);
    this.#local = local;
    this.#tabs = tabs;
    this.#jar = restored;
    return this;
  }
  async fetch(target, init = {}, { documentURL, kind = "fetch" } = {}) {
    if (!["fetch", "script", "module", "navigation"].includes(kind))
      throw new TypeError("Unsupported resource kind");
    let u = publicHttpURL(target, documentURL);
    // Fragments never leave the host, but are part of a navigated document URL.
    let fragment = new URL(target, documentURL).hash;
    if (!documentURL && kind !== "navigation")
      throw new TypeError("Document URL required");
    const docOrigin = documentURL ? origin(documentURL) : u.origin,
      credentials =
        init.credentials ?? (kind === "navigation" ? "include" : "same-origin");
    if (!["omit", "same-origin", "include"].includes(credentials))
      throw new TypeError("Invalid credentials");
    if (init.mode && !["cors", "same-origin", "no-cors"].includes(init.mode))
      throw new TypeError("Unsupported request mode");
    const headers = new Headers(init.headers);
    for (const n of headers.keys())
      if (
        forbidden.has(n) ||
        n.startsWith("sec-") ||
        n.startsWith("proxy-") ||
        n.startsWith("x-nanocodex-")
      )
        throw new TypeError("Forbidden page header");
    let method = String(init.method || "GET").toUpperCase(),
      body = init.body;
    if (
      !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"].includes(
        method,
      )
    )
      throw new TypeError("Invalid method");
    for (let hops = 0; hops <= 10; hops++) {
      const cross = u.origin !== docOrigin,
        classic =
          kind === "script" &&
          this.#allow &&
          init.mode !== "cors" &&
          init.mode !== "same-origin";
      const cors = cross && kind !== "navigation" && !classic;
      if (cors && ["same-origin", "no-cors"].includes(init.mode))
        throw new TypeError(
          "Cross-origin request mode unsupported and blocked",
        );
      if (cors)
        await preflight(
          this.#transport,
          u.href,
          method,
          headers,
          docOrigin,
          credentials,
          init.signal,
        );
      const cookies = credentials !== "omit" && !cross,
        outgoing = new Headers(headers);
      if (cookies) {
        const c = this.#jar.getCookieStringSync(u.href, {
          http: true,
          sameSiteContext: "strict",
        });
        if (c) outgoing.set("cookie", c);
      }
      if (cors) outgoing.set("origin", docOrigin);
      const r = await this.#transport.fetch(u.href, {
        method,
        headers: outgoing,
        body,
        redirect: "manual",
        credentials: "omit",
        signal: init.signal,
      });
      if (
        r.redirected ||
        r.type === "opaqueredirect" ||
        (r.url && publicHttpURL(r.url).href !== u.href)
      )
        throw new TypeError("Transport violated manual redirect policy");
      if (cors) {
        try {
          checkCors(r, docOrigin, credentials);
        } catch (error) {
          await r.body?.cancel();
          throw error;
        }
      }
      if (cookies) {
        if (
          r.headers.has("set-cookie") &&
          typeof r.headers.getSetCookie !== "function"
        )
          throw new TypeError("Separate cookie headers required");
        for (const c of r.headers.getSetCookie?.() || [])
          try {
            this.#setCookie(c, u.href, true);
          } catch {}
      }
      if (redirects.has(r.status) && r.headers.has("location")) {
        await r.body?.cancel();
        if (
          init.redirect === "error" ||
          init.redirect === "manual" ||
          hops === 10
        )
          throw new TypeError("Redirect not supported or limit exceeded");
        const location = r.headers.get("location");
        const destination = new URL(location, u);
        // Until redirect-tainted origins are modeled, fail closed on CORS
        // origin transitions (including a same-origin fetch redirecting out).
        if (
          kind !== "navigation" &&
          !classic &&
          destination.origin !== u.origin
        )
          throw new TypeError("Cross-origin fetch redirect unsupported");
        if (location.includes("#")) fragment = destination.hash;
        u = publicHttpURL(destination);
        if (
          (r.status === 303 && method !== "HEAD") ||
          ([301, 302].includes(r.status) && method === "POST")
        ) {
          method = "GET";
          body = undefined;
          headers.delete("content-type");
        }
        continue;
      }
      const exposed = cors
        ? exposedCorsHeaders(r.headers, credentials)
        : new Headers(r.headers);
      exposed.delete("set-cookie");
      exposed.delete("set-cookie2");
      const safe = new Response(r.body, {
        status: r.status,
        statusText: r.statusText,
        headers: exposed,
      });
      Object.defineProperties(safe, {
        url: { value: u.href + (kind === "navigation" ? fragment : "") },
        redirected: { value: hops > 0 },
        type: { value: cors ? "cors" : "basic" },
      });
      return safe;
    }
  }
}
export function attachPageSession({
  vm,
  run,
  url,
  tabId = "default",
  session,
}) {
  const callback = vm.newFunction(
    "__sessionStorageBridge",
    (kind, op, key, value) => {
      try {
        return vm.newString(
          JSON.stringify({
            value: session.storageOperation(
              url,
              tabId,
              vm.dump(kind),
              vm.dump(op),
              vm.dump(key),
              vm.dump(value),
            ),
          }),
        );
      } catch (e) {
        return vm.newString(JSON.stringify({ error: String(e.message) }));
      }
    },
  );
  vm.setProp(vm.global, "__sessionStorageBridge", callback);
  callback.dispose();
  run(
    `(()=>{const bridge=globalThis.__sessionStorageBridge;delete globalThis.__sessionStorageBridge;function storage(kind){const call=(op,key,value)=>{const r=JSON.parse(bridge(kind,op,key,value));if(r.error){const e=new Error(r.error);e.name=r.error==='QuotaExceededError'?'QuotaExceededError':'SecurityError';throw e;}return r.value;};const api={get length(){return call('entries').length;},key(index){return call('entries')[Number(index)>>>0]?.[0]??null;},getItem(k){return call('get',String(k));},setItem(k,v){call('set',String(k),String(v));},removeItem(k){call('remove',String(k));},clear(){call('clear');}};for(const n of Object.keys(api))Object.defineProperty(api,n,{enumerable:false});return new Proxy(api,{get(t,k){return k in t?Reflect.get(t,k):typeof k==='string'?call('get',k)??undefined:undefined;},set(t,k,v){if(k in t)return false;call('set',String(k),String(v));return true;},deleteProperty(t,k){if(k in t)return false;call('remove',String(k));return true;},ownKeys(){return call('entries').map(e=>e[0]).filter(k=>!(k in api));},getOwnPropertyDescriptor(t,k){if(k in t)return Reflect.getOwnPropertyDescriptor(t,k);const value=call('get',String(k));return value===null?undefined:{value,writable:true,enumerable:true,configurable:true};}});}Object.defineProperty(globalThis,'localStorage',{configurable:true,value:storage('local')});Object.defineProperty(globalThis,'sessionStorage',{configurable:true,value:storage('session')});})();`,
    "session-storage.js",
  );
}
