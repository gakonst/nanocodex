import { betterAuth } from "better-auth";
import { HostPrincipal } from "nanocodex/connect/server";

import {
  routeRequest,
  type AuthServer,
  type HostPrincipalIssuer,
  type RouterDependencies,
  type WorkerEnv,
} from "./router";

const dependencies: RouterDependencies = {
  auth(env) {
    return betterAuth({
      appName: "Nanocodex Better Auth example",
      basePath: "/api/auth",
      baseURL: env.NANOCODEX_HOST_APP_ORIGIN,
      database: env.AUTH_DB,
      secret: env.BETTER_AUTH_SECRET,
      trustedOrigins: [env.NANOCODEX_HOST_APP_ORIGIN!],
      socialProviders: {
        github: {
          clientId: env.BETTER_AUTH_GITHUB_CLIENT_ID!,
          clientSecret: env.BETTER_AUTH_GITHUB_CLIENT_SECRET!,
        },
      },
      account: {
        encryptOAuthTokens: true,
        storeStateStrategy: "cookie",
        storeAccountCookie: false,
      },
    }) as AuthServer;
  },
  principal(env, configuration) {
    return HostPrincipal.create({
      appId: configuration.appId,
      appOrigin: configuration.appOrigin,
      secret: configuration.hostProjectSecret,
    }) as HostPrincipalIssuer;
  },
};

export default {
  fetch(request: Request, env: WorkerEnv): Promise<Response> {
    return routeRequest(request, env, dependencies);
  },
};
