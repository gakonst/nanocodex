// Isolated benchmark deployment only. Production never imports this entrypoint.
import managed, { type Env } from '../src/index';
import { ensureAccount, createApiKey, type Principal } from '../src/account-auth';
export * from '../src/index';
type BenchmarkEnv = Env & { BENCHMARK_INIT_TOKEN: string; BENCHMARK_API_KEY: string; BENCHMARK_OPENAI_KEY: string; BENCHMARK_USER: string };
export default {
  async fetch(request: Request, env: BenchmarkEnv, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname !== '/__benchmark/init') return managed.fetch(request, env, ctx);
    if (request.method !== 'POST' || !env.BENCHMARK_INIT_TOKEN || request.headers.get('authorization') !== `Bearer ${env.BENCHMARK_INIT_TOKEN}`) return new Response('Not found', {status:404});
    await ensureAccount(env, env.BENCHMARK_USER, true);
    const account = await (await env.NANOCODEX_USERS.getByName(env.BENCHMARK_USER).fetch('https://user.internal/account')).json<{organizationId:string}>();
    const grant = await (await env.NANOCODEX_ORGANIZATIONS.getByName(account.organizationId).fetch(`https://organization.internal/resolve?userId=${env.BENCHMARK_USER}`)).json<Omit<Principal,'kind'|'userId'|'subjectId'|'credentialId'>>();
    await createApiKey(env, { ...grant, kind:'account_session', userId:env.BENCHMARK_USER, subjectId:`user:${env.BENCHMARK_USER}`, credentialId:'benchmark' }, 'isolated-performance-benchmark', {id:env.BENCHMARK_API_KEY.slice(9,21), token:env.BENCHMARK_API_KEY, createdAt:1789110000000});
    const bound = await env.NANOCODEX.fetch(`https://broker.internal/users/${env.BENCHMARK_USER}/credentials/openai`, {method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({api_key:env.BENCHMARK_OPENAI_KEY})});
    if (!bound.ok) return Response.json({error:'benchmark_provider_binding',status:bound.status},{status:502});
    return Response.json({ready:true});
  },
};
