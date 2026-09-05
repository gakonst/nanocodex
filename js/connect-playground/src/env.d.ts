/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_CONNECT_DIALOG_HOST?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
