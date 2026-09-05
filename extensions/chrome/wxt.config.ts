import { nanocodex } from "nanocodex-vite";
import { defineConfig } from "wxt";

export default defineConfig({
  modules: ["@wxt-dev/module-react"],
  vite: () => ({
    plugins: [nanocodex({ chatGpt: false })],
    resolve: {
      dedupe: ["react", "react-dom"],
    },
  }),
  manifest: {
    name: "Nanocodex for Chrome",
    description: "Preview and keep prompt-created, reversible site recipes.",
    version: "0.1.0",
    minimum_chrome_version: "116",
    incognito: "not_allowed",
    key: "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEApJKR9mIAspoMO7OCUB5T3gTWkoBpPozdwHob+8bOnkBhNtJQeCE2epOesGbD6o5gDzJS94DszhwDZgqj8k/XJqi+RohrbWFoAWjnzXZOP3JKxoh7u5O2K+SKPz68dCjAZuobRxcr4UYgNBfhkgjW9eGtAc8dXHJSocVXx4N6dIpgyOoiAqIIB97QVvQ9Nw7w3laqq8CEt2724hVEvO/ClCvJcnLKqMQd5JjS3bg+ZnPErSiykrWoEddvg7Wx5SwrXPOKCGv7UujkeP4m+3YVtwe1pqpVPbi/252nC7lWO/vXBCoNDXzWdmp3DDjhxx0WUrl1KU/H6rOiaR6+Ia15twIDAQAB",
    permissions: [
      "scripting",
      "sidePanel",
      "storage"
    ],
    optional_permissions: [
      "cookies"
    ],
    host_permissions: [
      "https://nanocodex-connect-api.gakonst.workers.dev/*",
      "http://*/*",
      "https://*/*"
    ],
    content_security_policy: {
      extension_pages: "script-src 'self' 'wasm-unsafe-eval'; object-src 'self'; worker-src 'self'; connect-src 'self' https://nanocodex-connect-api.gakonst.workers.dev wss://nanocodex-connect-api.gakonst.workers.dev"
    },
    action: {
      default_title: "Open Nanocodex"
    }
  }
});
