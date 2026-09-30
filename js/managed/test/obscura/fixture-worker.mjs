import { fixtures } from "./journey.mjs";
const storageFixtures = {
  "https://fixture.example.com/storage-a":
    "<!doctype html><title>Storage A</title>",
  "https://fixture.example.com/storage-b":
    "<!doctype html><title>Storage B</title>",
  "https://child.example.com/storage":
    "<!doctype html><title>Other Origin</title>",
};
let deniedWrites = 0;
export default {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/fixture.css")
      return new Response(".fixture { color: red; }", {
        headers: { "content-type": "text/css" },
      });
    if (url.pathname === "/frame-listener.js") {
      await new Promise((r) => setTimeout(r, 40));
      return new Response(
        'addEventListener("message",e=>{if(e.data==="parent-ready")parent.postMessage("child-ready","https://fixture.example.com")})',
        { headers: { "content-type": "text/javascript" } },
      );
    }
    if (url.pathname === "/cors-stats") return Response.json({ deniedWrites });
    if (url.pathname.startsWith("/cors-")) {
      const headers = new Headers({
        "access-control-allow-origin":
          url.pathname === "/cors-wrong"
            ? "https://unrelated.example.com"
            : "https://fixture.example.com",
        "access-control-allow-headers": "x-fixture",
        "access-control-allow-methods": "PUT",
        "access-control-expose-headers": "x-visible",
        "x-visible": "public",
        "x-hidden": "private",
        "content-type": "text/plain",
      });
      if (url.pathname === "/cors-wildcard")
        headers.set("access-control-allow-origin", "*");
      if (url.pathname === "/cors-credentials")
        headers.set("access-control-allow-credentials", "true");
      if (request.method === "OPTIONS") {
        if (url.pathname === "/cors-deny")
          headers.delete("access-control-allow-headers");
        return new Response(null, { status: 204, headers });
      }
      if (url.pathname === "/cors-deny") deniedWrites++;
      return new Response(
        JSON.stringify({
          method: request.method,
          origin: request.headers.get("origin"),
          header: request.headers.get("x-fixture"),
          body: request.method === "GET" ? null : await request.text(),
        }),
        { headers },
      );
    }
    if (url.pathname === "/redirect-fragment")
      return Response.redirect("https://child.example.com/frame", 302);
    if (url.pathname === "/redirect-new-fragment")
      return Response.redirect("https://child.example.com/frame#replaced", 302);

    if (request.url === "https://fixture.example.com/large.js") {
      return new Response(
        "/*" +
          "x".repeat(3 * 1024 * 1024) +
          "*/globalThis.largeScriptLoaded=true",
        { headers: { "content-type": "text/javascript" } },
      );
    }
    if (request.url === "https://fixture.example.com/no-content")
      return new Response(null, { status: 204 });
    if (request.url === "https://fixture.example.com/oversized") {
      let remaining = 17 * 1024 * 1024;
      return new Response(
        new ReadableStream({
          pull(controller) {
            if (!remaining) return controller.close();
            const size = Math.min(remaining, 64 * 1024);
            remaining -= size;
            controller.enqueue(new Uint8Array(size));
          },
        }),
      );
    }
    const body = fixtures[request.url] ?? storageFixtures[request.url];
    return new Response(body ?? "Not found", {
      status: body === void 0 ? 404 : 200,
      headers: {
        "content-type": request.url.endsWith(".js")
          ? "text/javascript"
          : "text/html",
      },
    });
  },
};
