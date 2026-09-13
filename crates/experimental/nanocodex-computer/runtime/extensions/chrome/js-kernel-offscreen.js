import { isKernelRequest, isKernelResponse, kernelError } from "./js-kernel-protocol.js";

// This is a packaged document entrypoint. No native or background caller is
// established by the captured source, and this module creates none.
export async function exchangeKernelMessage(frame, ready, request) {
  await ready;
  const destination = frame.contentWindow;
  if (destination == null) throw new Error("JavaScript sandbox is unavailable");
  return await new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = ({ data }) => {
      channel.port1.close();
      if (!isKernelResponse(data) || data.id !== request.id) {
        reject(new Error("JavaScript sandbox returned an invalid result"));
      } else {
        resolve(data);
      }
    };
    channel.port1.onmessageerror = () => {
      channel.port1.close();
      reject(new Error("JavaScript sandbox returned an unreadable result"));
    };
    try {
      destination.postMessage(request, "*", [channel.port2]);
    } catch (reason) {
      channel.port1.close();
      channel.port2.close();
      reject(reason);
    }
  });
}

export function installKernelBridge() {
  const frame = document.createElement("iframe");
  frame.hidden = true;
  const ready = new Promise((resolve, reject) => {
    const receiveReady = (event) => {
      if (event.source !== frame.contentWindow || event.data === null ||
          typeof event.data !== "object" || event.data.type !== "js-kernel:ready") return;
      window.clearTimeout(timeout);
      window.removeEventListener("message", receiveReady);
      resolve();
    };
    const timeout = window.setTimeout(() => {
      window.removeEventListener("message", receiveReady);
      const error = new Error("JavaScript sandbox did not initialize within 5 seconds");
      error.name = "JsKernelInitializationTimeoutError";
      reject(error);
    }, 5000);
    window.addEventListener("message", receiveReady);
  });
  chrome.runtime.onMessage.addListener((request, sender, reply) => {
    if (sender.id !== chrome.runtime.id || !isKernelRequest(request)) return false;
    exchangeKernelMessage(frame, ready, request).then(reply).catch((reason) => {
      reply({ id: request.id, ok: false, error: kernelError(reason) });
    });
    return true;
  });
  frame.src = chrome.runtime.getURL("js-kernel.html");
  document.body.append(frame);
}

installKernelBridge();
