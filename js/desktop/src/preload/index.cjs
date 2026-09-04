const { contextBridge, ipcRenderer } = require("electron");
const methods = [
  "state",
  "connect",
  "startSignIn",
  "verifySignIn",
  "cancelSignIn",
  "disconnect",
  "refresh",
  "openThread",
  "closeThread",
  "older",
  "createThread",
  "prompt",
  "steer",
  "cancel",
  "settings",
  "choosePath",
  "saveLayout",
  "saveHand",
  "prepareFolderHand",
  "startHand",
  "stopHand",
  "removeHand",
  "openAccount",
];
contextBridge.exposeInMainWorld("nanocodex", {
  ...Object.fromEntries(
    methods.map((name) => [
      name,
      (...args) => ipcRenderer.invoke(`nanocodex:${name}`, ...args),
    ])
  ),
  onEvent(listener) {
    const handler = (_event, value) => listener(value);
    ipcRenderer.on("nanocodex:event", handler);
    return () => ipcRenderer.removeListener("nanocodex:event", handler);
  },
});
