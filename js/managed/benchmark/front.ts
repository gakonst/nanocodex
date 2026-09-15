// Reuse the production account-to-managed service hop.
import {routeManaged, type ManagedProxyEnv} from '../../account/worker/managedProxy';
export default {
  async fetch(request: Request, env: ManagedProxyEnv): Promise<Response> {
    return await routeManaged(request, env, new URL(request.url)) ?? new Response('Not found',{status:404});
  },
};
