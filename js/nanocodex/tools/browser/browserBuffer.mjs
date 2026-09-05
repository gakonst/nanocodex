import * as bufferModule from "buffer";

// CommonJS interop differs between Vite's page optimizer, Worker graph, and
// production bundler. A namespace import requests no synthetic binding; read
// the constructor from whichever CJS namespace shape that graph provides.
const buffer = bufferModule.Buffer === undefined ? bufferModule.default : bufferModule;
export const { Buffer } = buffer;
if (typeof Buffer !== "function") {
  throw new Error("the browser Buffer compatibility module did not expose Buffer");
}
// isomorphic-git still reads Buffer from the global scope in its browser build.
// Install it in every browser/Worker realm before a Git operation can run.
globalThis.Buffer ??= Buffer;
// A few CommonJS browser libraries (notably the SSH stream dependencies) use
// Node's `global` spelling even when their implementation selects Web Crypto.
const commonJsGlobal = globalThis;
commonJsGlobal.global ??= globalThis;
commonJsGlobal.process ??= { versions: {} };
