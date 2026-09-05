import { routeHostPrincipal, type Env as HostAuthEnv } from "./host-principal";

type Env = HostAuthEnv & Readonly<{
  ASSETS: Readonly<{ fetch(request: Request): Promise<Response> }>;
}>;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const response = await routeHostPrincipal(request, env);
    return response ?? env.ASSETS.fetch(request);
  },
};
