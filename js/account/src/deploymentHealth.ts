export type DeploymentCredentialSource = "brokered" | "sponsored" | null;

export type DeploymentHealth = Readonly<{
  agentConfigured: boolean;
  astraEntitled: boolean;
  credentialSource: DeploymentCredentialSource;
  deploymentSha: string | undefined;
  freePromptsRemaining: number | null;
  voiceEnabled: boolean;
}>;

type HealthPayload = {
  agent_configured?: unknown;
  astra_entitled?: unknown;
  credential_source?: unknown;
  deployment_sha?: unknown;
  free_prompts_remaining?: unknown;
  voice_enabled?: unknown;
};

/** One app-owned, single-flight view of the Worker health boundary. */
export function createDeploymentHealthResource(
  fetchHealth: typeof fetch = globalThis.fetch.bind(globalThis),
) {
  let cached: DeploymentHealth | undefined;
  let epoch = 0;
  let inFlight: Promise<DeploymentHealth> | undefined;

  const request = () => {
    if (inFlight) return inFlight;
    const requestEpoch = epoch;
    const current = fetchHealth("/api/health", {
      cache: "no-store",
      credentials: "same-origin",
    }).then(async (response) => {
      if (!response.ok) {
        throw new Error(`Could not check the agent session (HTTP ${response.status})`);
      }
      const payload = await response.json() as HealthPayload;
      const sponsoredRemaining = Number.isSafeInteger(payload.free_prompts_remaining)
        && (payload.free_prompts_remaining as number) >= 0
        && (payload.free_prompts_remaining as number) <= 3
        ? payload.free_prompts_remaining as number
        : null;
      const credentialSource = payload.agent_configured !== true
        ? null
        : payload.credential_source === "brokered" || payload.credential_source === "subscription"
          ? "brokered"
          : payload.credential_source === "sponsored" && sponsoredRemaining !== null
            ? "sponsored"
            : payload.credential_source === "user" ? "brokered" : null;
      return Object.freeze({
        agentConfigured: credentialSource !== null,
        astraEntitled: credentialSource === "brokered" && payload.astra_entitled === true,
        credentialSource,
        deploymentSha: typeof payload.deployment_sha === "string"
          ? payload.deployment_sha
          : undefined,
        freePromptsRemaining: credentialSource === "sponsored" ? sponsoredRemaining : null,
        voiceEnabled: credentialSource !== "sponsored" && payload.voice_enabled === true,
      });
    });
    inFlight = current;
    void current.then(
      (health) => {
        if (inFlight === current) {
          if (epoch === requestEpoch) cached = health;
          inFlight = undefined;
        }
      },
      () => {
        if (inFlight === current) inFlight = undefined;
      },
    );
    return current;
  };

  return Object.freeze({
    read(): Promise<DeploymentHealth> {
      return cached ? Promise.resolve(cached) : request();
    },
    refresh(): Promise<DeploymentHealth> {
      return request();
    },
    invalidate(): void {
      epoch += 1;
      cached = undefined;
      inFlight = undefined;
    },
  });
}

export const deploymentHealth = createDeploymentHealthResource();
