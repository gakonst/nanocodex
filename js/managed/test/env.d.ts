import type { Env as WorkerEnv } from "../src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {}
}
