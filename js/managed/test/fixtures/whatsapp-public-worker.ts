import { Kv } from "accounts/server";
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, authenticate } from "../../src/account-auth";
import { routeConnectorRequest } from "../../src/connectors";
import { accountConnectorsTool } from "../../src/account-connectors-tool";
import { connectorToolsProvider } from "../../src/connector-tools";
import { handleManagedEgress, exactConnectorAccess } from "../../src/managed-egress";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
// Synthetic identity enrollment only; shipped authentication and routing stay real.
export default { async fetch(request: Request, env: any) {
  const url = new URL(request.url);
  if (url.pathname === "/__fixture/account-connectors") {
    const principal = await authenticate(request, env, url);
    if (!principal) return Response.json({ error: "unauthorized" }, { status: 401 });
    const input: any = await request.json();
    const tool = accountConnectorsTool(() => ({
      broker: env.NANOCODEX, userId: principal.userId, sessionId: "synthetic-session", publicOrigin: url.origin,
      canManage: () => !principal.connectGrant && principal.capabilities.includes("organization:write"),
      allowedConnectors: () => principal.connectGrant ? [] : undefined,
    }));
    try {
      return Response.json(await tool.handler(input.request, {
        signal: request.signal,
        ...(input.subagent ? { subagent: { agentId: "synthetic-child" } } : {}),
      } as any));
    } catch (error) { return Response.json({ error: String(error) }, { status: 400 }); }
  }
  if (url.pathname === "/__fixture/tool" || url.pathname === "/__fixture/egress") {
    const input: any = await request.json();
    const allowed = (_capability: string, selected?: string) => input.grant === false ? false : exactConnectorAccess(["c".repeat(43)], selected);
    const subject = input.subject === false ? undefined : "s".repeat(43);
    if (url.pathname === "/__fixture/egress") return handleManagedEgress(new Request(input.url, {headers:input.headers}), env.NANOCODEX, subject, allowed);
    const provider = connectorToolsProvider({available: capability => capability === "whatsapp" && input.available !== false,
      fetch: (r) => handleManagedEgress(r, env.NANOCODEX, subject, allowed)});
    const tool = provider.resolve("whatsapp_request");
    if (!tool) return Response.json({error:"tool_unavailable"}, {status:403});
    try { return Response.json(await tool.handler(input.request, {signal:request.signal} as any)); }
    catch (error) { return Response.json({error:String(error)}, {status:400}); }
  }
  if (url.pathname === "/__fixture/principal") {
    const principal = await authenticate(request, env, url);
    return Response.json({kind:principal?.kind ?? null});
  }
  if (url.pathname === "/__fixture") {
    const input: any = await request.json();
    await ensureAccount(env, input.user, true);
    const auth: any = await (await env.NANOCODEX_USERS.getByName(input.user).fetch("https://user.internal/authorization")).json();
    const key = await createApiKey(env, {kind:"api_key", userId:input.user, ...auth.grant,
      subjectId:`api_key:${input.user}`, credentialId:"fixture", capabilities:input.capabilities ?? auth.grant.capabilities}, "synthetic-whatsapp");
    const token = "s_" + "x".repeat(43);
    await Kv.durableObject(env.NANOCODEX_AUTH, {name:"account"}).set(`session:${token}`, {
      authentication:"sms_otp", userId:input.user, issuedAt:Date.now()/1000, expiresAt:Date.now()/1000+600,
    });
    return Response.json({...key, cookie:`nanocodex_account=${token}`});
  }
  return await routeConnectorRequest(request, env, url) ?? new Response(null, {status:404});
}};
