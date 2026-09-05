import assert from "node:assert/strict";
import { test } from "node:test";

import { Dialog } from "../cloud/index.mjs";

const DEFAULT_HOST = "https://nanocodex.gakonst.workers.dev/connect-dialog/";
const DEFAULT_ORIGIN = "https://nanocodex.gakonst.workers.dev";

test("the default Connect dialog stays embedded and accepts responses only from its iframe origin", async () => {
  const browser = createBrowserHarness();
  const previousDocument = globalThis.document;
  const previousWindow = globalThis.window;
  globalThis.document = browser.document;
  globalThis.window = browser.window;

  try {
    assert.equal(Dialog.DEFAULT_HOST, DEFAULT_HOST);
    const dialog = Dialog.iframe().setup({ appId: "embedded-test" });
    const source = new URL(dialog.host);
    assert.equal(source.origin, DEFAULT_ORIGIN);
    assert.equal(source.pathname, "/connect-dialog/");
    assert.equal(source.searchParams.get("app_id"), "embedded-test");
    assert.equal(source.searchParams.get("origin"), "https://consumer.example");
    assert.equal(source.searchParams.get("mode"), "iframe");

    const request = { id: "request-1", type: "connect" };
    const result = dialog.open(request);
    const modal = browser.document.body.children.find(
      (element) => element.attributes.get("aria-label") === "Nanocodex Connect permissions",
    );
    const frame = modal.children[0];

    assert.equal(modal.tagName, "DIALOG");
    assert.equal(frame.tagName, "IFRAME");
    assert.equal(frame.src, dialog.host);
    assert.match(frame.allow, new RegExp(`publickey-credentials-get ${DEFAULT_ORIGIN}`));

    frame.dispatch("load");
    await Promise.resolve();
    assert.deepEqual(frame.contentWindow.messages, [{
      message: { type: "nanocodex:request", id: request.id, request },
      targetOrigin: DEFAULT_ORIGIN,
    }]);

    let settled = false;
    result.then(() => { settled = true; });
    const response = {
      type: "nanocodex:response",
      id: request.id,
      result: { approved: true },
    };
    browser.window.dispatchMessage({
      data: response,
      origin: "https://attacker.example",
      source: frame.contentWindow,
    });
    browser.window.dispatchMessage({
      data: response,
      origin: DEFAULT_ORIGIN,
      source: {},
    });
    await Promise.resolve();
    assert.equal(settled, false);

    browser.window.dispatchMessage({
      data: response,
      origin: DEFAULT_ORIGIN,
      source: frame.contentWindow,
    });
    assert.deepEqual(await result, { approved: true });
  } finally {
    globalThis.document = previousDocument;
    globalThis.window = previousWindow;
  }
});

test("the popup Connect dialog opens the canonical host as a top-level wallet target", () => {
  const previousWindow = globalThis.window;
  const opened = [];
  const popupWindow = {
    closed: false,
    focusCount: 0,
    close() { this.closed = true; },
    focus() { this.focusCount += 1; },
  };
  globalThis.window = {
    location: { origin: "chrome-extension://extension-id" },
    open(source, target, features) {
      opened.push({ source, target, features });
      return popupWindow;
    },
  };

  try {
    const dialog = Dialog.popup().setup({ appId: "popup-test" });
    dialog.showWallet();
    assert.equal(opened.length, 1);
    assert.equal(opened[0].target, "nanocodex-connect");
    assert.match(opened[0].features, /popup=yes/);
    const source = new URL(opened[0].source);
    assert.equal(source.origin, DEFAULT_ORIGIN);
    assert.equal(source.pathname, "/connect-dialog/");
    assert.equal(source.searchParams.get("app_id"), "popup-test");
    assert.equal(source.searchParams.get("origin"), "chrome-extension://extension-id");
    assert.equal(source.searchParams.get("mode"), "popup");
    assert.equal(dialog.host, source.toString());
    assert.equal(dialog.walletTarget(), popupWindow);

    dialog.showWallet();
    assert.equal(opened.length, 1);
    assert.equal(popupWindow.focusCount, 2);
    dialog.hideWallet();
    assert.equal(popupWindow.closed, true);
    assert.equal(dialog.walletTarget(), undefined);
  } finally {
    globalThis.window = previousWindow;
  }
});

test("popup URLs overwrite caller-controlled routing parameters", () => {
  const previousWindow = globalThis.window;
  const opened = [];
  globalThis.window = {
    location: { origin: "https://consumer.example" },
    open(source) {
      opened.push(source);
      return { closed: false, focus() {} };
    },
  };

  try {
    const dialog = Dialog.popup({
      host: `${DEFAULT_HOST}?app_id=attacker&origin=https://attacker.example&mode=iframe`,
    }).setup({ appId: "consumer-example" });
    dialog.showWallet();
    const source = new URL(opened[0]);
    assert.equal(source.searchParams.get("app_id"), "consumer-example");
    assert.equal(source.searchParams.get("origin"), "https://consumer.example");
    assert.equal(source.searchParams.get("mode"), "popup");
  } finally {
    globalThis.window = previousWindow;
  }
});

function createBrowserHarness() {
  const windowListeners = new Map();
  const body = createElement("body");
  const document = {
    body,
    createElement(tagName) {
      const element = createElement(tagName);
      if (tagName === "iframe") {
        element.contentWindow = {
          messages: [],
          postMessage(message, targetOrigin) {
            this.messages.push({ message, targetOrigin });
          },
        };
      }
      return element;
    },
  };
  const window = {
    location: { origin: "https://consumer.example" },
    addEventListener(type, listener) {
      windowListeners.set(type, listener);
    },
    dispatchMessage(event) {
      windowListeners.get("message")?.(event);
    },
  };
  return { document, window };
}

function createElement(tagName) {
  const listeners = new Map();
  return {
    attributes: new Map(),
    children: [],
    dataset: {},
    open: false,
    style: {},
    tagName: tagName.toUpperCase(),
    addEventListener(type, listener) {
      listeners.set(type, listener);
    },
    append(...children) {
      this.children.push(...children);
    },
    close() {
      this.open = false;
    },
    dispatch(type, event = {}) {
      listeners.get(type)?.(event);
    },
    focus() {},
    remove() {},
    removeAttribute(name) {
      this.attributes.delete(name);
    },
    setAttribute(name, value) {
      this.attributes.set(name, value);
    },
    showModal() {
      this.open = true;
    },
  };
}
