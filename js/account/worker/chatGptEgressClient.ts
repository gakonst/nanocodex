const CHATGPT_ORIGIN = "https://chatgpt.com";
const EGRESS_ORIGIN = "https://chatgpt-egress.internal";
const SESSION_ID_PATTERN = /^[A-Za-z0-9_-]{43}$/;

export type ChatGptEgressEnv = {
  ENVIRONMENT: string;
  CHATGPT_EGRESS?: DurableObjectNamespace;
};

export function fetchChatGpt(
  env: ChatGptEgressEnv,
  input: string | URL,
  init?: RequestInit,
  sessionId?: string,
): Promise<Response> {
  const target = new URL(input);
  if (env.ENVIRONMENT !== "production" && env.ENVIRONMENT !== "preview") {
    return fetch(target, init);
  }
  if (target.origin !== CHATGPT_ORIGIN) {
    return Promise.reject(new Error("ChatGPT egress only accepts chatgpt.com URLs"));
  }
  if (!env.CHATGPT_EGRESS) {
    return Promise.reject(new Error("ChatGPT Container egress is not configured"));
  }
  if (!sessionId || !SESSION_ID_PATTERN.test(sessionId)) {
    return Promise.reject(new Error("ChatGPT egress requires a valid session ID"));
  }
  const internal = new URL(`${target.pathname}${target.search}`, EGRESS_ORIGIN);
  return egressStub(env.CHATGPT_EGRESS, sessionId).fetch(new Request(internal, init));
}

export async function warmChatGptEgress(env: ChatGptEgressEnv, sessionId: string): Promise<void> {
  if (
    !env.CHATGPT_EGRESS
    || (env.ENVIRONMENT !== "production" && env.ENVIRONMENT !== "preview")
  ) return;
  if (!SESSION_ID_PATTERN.test(sessionId)) throw new Error("invalid ChatGPT session ID");
  const response = await egressStub(env.CHATGPT_EGRESS, sessionId).fetch(
    new Request(`${EGRESS_ORIGIN}/health`),
  );
  await response.body?.cancel();
  if (!response.ok) throw new Error(`ChatGPT Container egress health check failed: ${response.status}`);
}

function egressStub(namespace: DurableObjectNamespace, sessionId: string): DurableObjectStub {
  return namespace.get(namespace.idFromName(`session-v2:${sessionId}`));
}
