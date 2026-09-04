import {
  app,
  BrowserWindow,
  dialog,
  ipcMain,
  Menu,
  nativeImage,
  nativeTheme,
  net,
  protocol,
  safeStorage,
  shell,
} from "electron";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, join, resolve, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseEnv } from "node:util";
import { DesktopRuntime, DEFAULT_ORIGIN } from "@nanocodex/desktop-runtime";
import { desktopDefaults } from "@nanocodex/desktop-runtime/configuration";
import { SmsSignIn } from "@nanocodex/desktop-runtime/auth";

const here = dirname(fileURLToPath(import.meta.url));
app.setName("Nanocodex");
process.title = "Nanocodex";
if (process.env.NANOCODEX_DESKTOP_DATA)
  app.setPath("userData", process.env.NANOCODEX_DESKTOP_DATA);
if (!app.requestSingleInstanceLock()) app.exit(0);
app.on("second-instance", () => {
  if (window) {
    if (window.isMinimized()) window.restore();
    window.show();
    window.focus();
  }
});
protocol.registerSchemesAsPrivileged([
  {
    scheme: "nanocodex",
    privileges: { standard: true, secure: true, supportFetchAPI: true },
  },
]);
let window;
let runtime;
let quitting = false;
const developmentUrl = !app.isPackaged
  ? process.env.ELECTRON_RENDERER_URL
  : undefined;
const home = "nanocodex://app/index.html";

app
  .whenReady()
  .then(async () => {
    if (
      process.env.NANOCODEX_DESKTOP_TEST === "1" &&
      process.platform === "darwin"
    ) {
      app.dock.hide();
      app.hide();
    }
    const iconPath = app.isPackaged
      ? join(process.resourcesPath, "icon.png")
      : resolve(here, "../../../../assets/nanocodex/icon.png");
    const icon = nativeImage.createFromPath(iconPath);
    if (process.platform === "darwin" && !icon.isEmpty())
      app.dock.setIcon(icon);
    app.setAboutPanelOptions({
      applicationName: "Nanocodex",
      applicationVersion: "0.1.0",
      version: "0.1.0",
      copyright: "Copyright © 2026 Nanocodex",
      iconPath,
      website: "https://github.com/gakonst/nanocodex",
    });
    const storePath = join(app.getPath("userData"), "desktop.json");
    let store = {};
    try {
      const saved = JSON.parse(await readFile(storePath, "utf8"));
      if (saved && typeof saved === "object" && !Array.isArray(saved))
        store = saved;
    } catch {
      /* First launch. */
    }
    let environment = process.env;
    if (!app.isPackaged || process.env.NANOCODEX_ENV_FILE) {
      const path =
        process.env.NANOCODEX_ENV_FILE || resolve(here, "../../../..", ".env");
      try {
        environment = {
          ...parseEnv(await readFile(path, "utf8")),
          ...process.env,
          NANOCODEX_ENV_FILE: path,
        };
      } catch {
        /* Environment configuration is optional. */
      }
    }
    let apiKey = environment.NANOCODEX_API_KEY || environment.NC_API_KEY;
    let baseUrl = environment.NANOCODEX_MANAGED_URL || DEFAULT_ORIGIN;
    let importedConnection =
      apiKey && app.isPackaged && process.env.NANOCODEX_DESKTOP_TEST !== "1"
        ? { apiKey, baseUrl, remember: true }
        : undefined;
    let accountEdited = false;
    if (!apiKey && store.connection && safeStorage.isEncryptionAvailable()) {
      try {
        const connection = JSON.parse(
          safeStorage.decryptString(Buffer.from(store.connection, "base64"))
        );
        apiKey = connection.apiKey;
        baseUrl = connection.baseUrl;
      } catch {
        /* Show sign-in when the OS keyring is unavailable. */
      }
    }
    const scopeFor = (key, origin) =>
      createHash("sha256").update(`${origin}\0${key}`).digest("hex");
    let scope = apiKey ? scopeFor(apiKey, baseUrl) : undefined;
    let saving = Promise.resolve();
    const saveStore = (update) => {
      saving = saving
        .catch(() => {})
        .then(async () => {
          // Apply updates inside the write queue. A failed write must leave the
          // in-memory account unchanged, including later window/preferences saves.
          const next = update(store);
          const contents = JSON.stringify(next);
          await mkdir(dirname(storePath), { recursive: true, mode: 0o700 });
          await writeFile(`${storePath}.tmp`, contents, { mode: 0o600 });
          await rename(`${storePath}.tmp`, storePath);
          store = next;
        });
      return saving;
    };
    let signIn;
    const defaults = await desktopDefaults(environment);
    runtime = new DesktopRuntime({
      apiKey,
      baseUrl,
      dataDirectory: app.getPath("userData"),
      defaults,
      saved: store.scope === scope ? store.preferences : {},
      persist: async (preferences) => {
        await saveStore((current) => ({ ...current, scope, preferences }));
      },
      saveConnection: async (connection) => {
        let encrypted;
        if (connection?.remember) {
          if (
            !safeStorage.isEncryptionAvailable() ||
            (process.platform === "linux" &&
              safeStorage.getSelectedStorageBackend() === "basic_text")
          )
            throw new Error(
              "OS credential storage is unavailable. Connect without remembering the key."
            );
          encrypted = safeStorage
            .encryptString(JSON.stringify(connection))
            .toString("base64");
        }
        const nextScope = connection
          ? scopeFor(connection.apiKey, connection.baseUrl)
          : undefined;
        try {
          await saveStore((current) => {
            const next = { ...current };
            if (encrypted) next.connection = encrypted;
            else delete next.connection;
            return next;
          });
        } catch {
          throw new Error("Nanocodex could not save your sign-in. Check available disk space and try again.");
        }
        scope = nextScope;
        if (connection && signIn?.credential === connection.apiKey)
          signIn.persisted = true;
      },
    });
    apiKey = undefined;

    protocol.handle("nanocodex", (request) => {
      const url = new URL(request.url);
      if (url.host !== "app" || request.method !== "GET")
        return new Response("Not found", { status: 404 });
      const root = resolve(here, "../renderer");
      const target = resolve(root, `.${decodeURIComponent(url.pathname)}`);
      if (relative(root, target).startsWith(".."))
        return new Response("Not found", { status: 404 });
      return net.fetch(pathToFileURL(target).href);
    });

    function trusted(event) {
      if (
        !window ||
        event.sender !== window.webContents ||
        event.senderFrame !== window.webContents.mainFrame
      )
        return false;
      const source = new URL(event.senderFrame.url);
      return developmentUrl
        ? source.origin === new URL(developmentUrl).origin
        : source.protocol === "nanocodex:" && source.host === "app";
    }
    let accountOperation = Promise.resolve();
    const accountAction = (action) => {
      const operation = accountOperation.catch(() => {}).then(() => {
        if (quitting) throw new Error("Nanocodex is closing.");
        return action();
      });
      accountOperation = operation;
      return operation;
    };
    const cancelSignIn = async () => {
      const attempt = signIn;
      if (attempt?.persisted) await attempt.auth.complete();
      else await attempt?.auth.cancel();
      signIn = undefined;
    };
    const requireSecureStorage = () => {
      if (!safeStorage.isEncryptionAvailable() ||
          (process.platform === "linux" && safeStorage.getSelectedStorageBackend() === "basic_text"))
        throw new Error("Secure storage is unavailable. Unlock your system keychain and try again.");
    };
    const actions = {
      state: () => runtime.state(),
      startSignIn: (value) => accountAction(async () => {
        requireSecureStorage();
        const origin = value?.baseUrl || runtime.state().baseUrl;
        if (signIn && signIn.origin !== origin) await cancelSignIn();
        if (!signIn) signIn = { origin, auth: new SmsSignIn({ baseUrl: origin }) };
        return signIn.auth.start({ phone: value?.phone });
      }),
      verifySignIn: (value) => accountAction(async () => {
        if (!signIn) throw new Error("Request a text message code first.");
        requireSecureStorage();
        accountEdited = true;
        const credential = await signIn.auth.verify({ code: value?.code });
        signIn.credential = credential.apiKey;
        const state = await runtime.connect({ ...credential, remember: true });
        // Only the OS-encrypted credential store receives the new API key.
        // Mark it consumed before ending the temporary browser session.
        const completed = signIn;
        signIn = undefined;
        await completed.auth.complete();
        return state;
      }),
      cancelSignIn: () => accountAction(cancelSignIn),
      connect: (value) => accountAction(async () => {
        accountEdited = true;
        if (
          value.remember &&
          (!safeStorage.isEncryptionAvailable() ||
            (process.platform === "linux" &&
              safeStorage.getSelectedStorageBackend() === "basic_text"))
        )
          throw new Error(
            "Secure storage is unavailable. Uncheck Remember to connect for this session."
          );
        await cancelSignIn();
        return runtime.connect(value);
      }),
      disconnect: () => accountAction(async () => {
        accountEdited = true;
        await cancelSignIn();
        return runtime.disconnect();
      }),
      refresh: () => runtime.refresh(),
      openThread: (id) => runtime.openThread(id),
      closeThread: (id) => runtime.closeThread(id),
      older: (id) => runtime.older(id),
      createThread: (settings) => runtime.createThread(settings),
      prompt: (value) => runtime.prompt(value),
      steer: (value) => runtime.steer(value),
      cancel: (value) => runtime.cancel(value),
      settings: (value) => runtime.settings(value),
      choosePath: async (kind) => {
        if (!["directory", "file"].includes(kind))
          throw new Error("Invalid selection type.");
        const result = await dialog.showOpenDialog(window, {
          title:
            kind === "directory"
              ? "Choose a workspace folder"
              : "Choose a file",
          properties:
            kind === "directory"
              ? ["openDirectory", "createDirectory"]
              : ["openFile"],
        });
        return result.canceled ? null : result.filePaths[0];
      },
      saveLayout: async (value) => {
        await runtime.saveLayout(value);
        nativeTheme.themeSource = value.theme;
      },
      saveHand: (value) => runtime.saveHand(value),
      prepareFolderHand: (value) => runtime.prepareFolderHand(value),
      startHand: (id) => runtime.startHand(id),
      stopHand: (id) => runtime.stopHand(id),
      removeHand: (id) => runtime.removeHand(id),
      openAccount: () =>
        shell.openExternal(new URL("/connect", runtime.state().baseUrl).href),
    };
    for (const [name, handler] of Object.entries(actions))
      ipcMain.handle(`nanocodex:${name}`, async (event, ...args) => {
        if (!trusted(event)) throw new Error("Untrusted desktop caller.");
        try {
          return await handler(...args);
        } catch (error) {
          throw new Error(
            String(error?.message ?? "Operation failed")
              .replace(/ncx_live_[A-Za-z0-9_-]+/g, "[redacted]")
              .slice(0, 500)
          );
        }
      });
    runtime.on("event", (event) => {
      if (window && !window.isDestroyed())
        window.webContents.send("nanocodex:event", event);
    });

    function createWindow() {
      const savedBounds =
        Number.isFinite(store.window?.width) &&
        Number.isFinite(store.window?.height) &&
        store.window.width >= 760 &&
        store.window.height >= 560
          ? {
              width: store.window.width,
              height: store.window.height,
              ...(Number.isFinite(store.window.x) &&
              Number.isFinite(store.window.y)
                ? { x: store.window.x, y: store.window.y }
                : {}),
            }
          : {};
      window = new BrowserWindow({
        width: 1360,
        height: 900,
        minWidth: 760,
        minHeight: 560,
        title: "Nanocodex",
        backgroundColor: "#f9f9f9",
        icon,
        ...savedBounds,
        show: false,
        titleBarStyle: "hiddenInset",
        trafficLightPosition: { x: 18, y: 20 },
        webPreferences: {
          preload: join(here, "../preload/index.cjs"),
          contextIsolation: true,
          nodeIntegration: false,
          sandbox: true,
        },
      });
      if (
        ["system", "light", "dark"].includes(store.preferences?.layout?.theme)
      )
        nativeTheme.themeSource = store.preferences.layout.theme;
      window.once("ready-to-show", () => {
        if (process.env.NANOCODEX_DESKTOP_TEST !== "1") window?.show();
        else if (process.platform === "darwin") app.hide();
      });
      window.on("close", () => {
        if (window && !window.isFullScreen()) {
          const bounds = window.getNormalBounds();
          void saveStore((current) => ({ ...current, window: bounds })).catch(() => {});
        }
      });
      window.webContents.session.setPermissionRequestHandler(
        (_contents, _permission, callback) => callback(false)
      );
      window.webContents.setWindowOpenHandler(({ url }) => {
        if (/^https?:\/\//.test(url)) void shell.openExternal(url);
        return { action: "deny" };
      });
      window.webContents.on("context-menu", (_event, params) => {
        const template = params.isEditable
          ? [
              { role: "undo" },
              { role: "redo" },
              { type: "separator" },
              { role: "cut" },
              { role: "copy" },
              { role: "paste" },
              { role: "selectAll" },
            ]
          : params.selectionText
          ? [{ role: "copy" }]
          : [];
        if (template.length) Menu.buildFromTemplate(template).popup({ window });
      });
      window.webContents.on("will-navigate", (event, url) => {
        if (
          url !== home &&
          (!developmentUrl ||
            new URL(url).origin !== new URL(developmentUrl).origin)
        )
          event.preventDefault();
      });
      window.on("closed", () => {
        window = undefined;
      });
      void window.loadURL(developmentUrl || home);
    }
    const command = (name) => () =>
      window?.webContents.send("nanocodex:event", {
        type: "command",
        command: name,
      });
    Menu.setApplicationMenu(
      Menu.buildFromTemplate([
        {
          label: "Nanocodex",
          submenu: [
            { role: "about" },
            { type: "separator" },
            { role: "hide" },
            { role: "hideOthers" },
            { type: "separator" },
            { role: "quit" },
          ],
        },
        {
          label: "File",
          submenu: [
            {
              label: "New Tab",
              accelerator: "CmdOrCtrl+T",
              click: command("new-tab"),
            },
            {
              label: "Close Tab",
              accelerator: "CmdOrCtrl+W",
              click: command("close-tab"),
            },
            {
              label: "Reopen Closed Tab",
              accelerator: "CmdOrCtrl+Shift+T",
              click: command("reopen-tab"),
            },
            { type: "separator" },
            {
              label: "Choose Folder…",
              accelerator: "CmdOrCtrl+O",
              click: command("choose-folder"),
            },
            {
              label: "Settings…",
              accelerator: "CmdOrCtrl+,",
              click: command("settings"),
            },
          ],
        },
        { role: "editMenu" },
        { role: "viewMenu" },
        { role: "windowMenu" },
      ])
    );
    createWindow();
    void runtime.refresh().then(async (state) => {
      try {
        if (
          importedConnection &&
          state.connected &&
          !accountEdited &&
          !store.connection &&
          safeStorage.isEncryptionAvailable() &&
          (process.platform !== "linux" ||
            safeStorage.getSelectedStorageBackend() !== "basic_text")
        ) {
          const encrypted = safeStorage
            .encryptString(JSON.stringify(importedConnection))
            .toString("base64");
          await saveStore((current) => accountEdited || current.connection
            ? current : { ...current, connection: encrypted });
        }
      } catch {
        console.warn("Could not remember this account in OS secure storage.");
      } finally {
        importedConnection = undefined;
      }
    });
    app.on("activate", () => {
      if (!window) createWindow();
    });
    app.on("window-all-closed", () => {
      if (process.platform !== "darwin") app.quit();
    });
    app.on("before-quit", (event) => {
      if (quitting) return;
      event.preventDefault();
      quitting = true;
      void accountOperation.catch(() => {})
        .then(cancelSignIn)
        .finally(() => runtime.close())
        .finally(async () => {
          await saving.catch(() => {});
          app.quit();
        })
        .catch(() => {});
    });
  })
  .catch((error) => {
    console.error(
      String(error?.message ?? error).replace(
        /ncx_live_[A-Za-z0-9_-]+/g,
        "[redacted]"
      )
    );
    app.exit(1);
  });
