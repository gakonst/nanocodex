import { apiKeyDigest, apiKeyPrincipal, type StoredApiKey, type ApiKeyPrincipal } from "nanocodex/cloudflare/managed-auth";
import { liveAgentFailure, liveAgentRequest, liveAgentSettings, newManagedAgentId } from "nanocodex/cloudflare/managed-live";
import { parseAgentSettingsQuery, type ManagedAgentSettings } from "nanocodex/cloudflare/agent-settings";
const request = new Request("https://account.test/v1/agents/live");
const digest = await apiKeyDigest(request);
const record = {} as StoredApiKey;
if (digest) {
  const principal: ApiKeyPrincipal | undefined = apiKeyPrincipal(record, digest);
  const settings = liveAgentSettings(request);
  const failure: Response | undefined = liveAgentFailure(request, principal);
  if (principal && !(settings instanceof Response) && !failure) {
    const internal: Request = liveAgentRequest(request, principal, settings, newManagedAgentId(), "FRA");
    void internal;
  }
}
const settings: ManagedAgentSettings = parseAgentSettingsQuery(new URLSearchParams());
void settings;
