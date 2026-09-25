/** Workerd multi-Worker fixture; the real container transport has a separate contract test. */
export function relayChatGpt(request: Request): Promise<Response> { return fetch(request); }
export function routeChatGpt(request: Request): Promise<Response> { return fetch(request); }
