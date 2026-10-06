/** Display the exact capability subset carried by the signed Connect resource. */
export type ServiceCapabilities = Readonly<{
  vault?: Readonly<{ ids: readonly string[]; origins: readonly string[]; request: boolean }>;
  phone?: Readonly<{ numberIds: readonly string[]; read: boolean; provision: boolean; release: boolean }>;
}>;
const prefix = "urn:nanocodex:services:";
const record = (value: unknown): value is Record<string, unknown> => Boolean(value) && typeof value === "object" && !Array.isArray(value);
const ids = (value: unknown): value is string[] => Array.isArray(value) && value.length <= 64 && new Set(value).size === value.length && value.every(id => typeof id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(id));
export function servicesFromResources(resources: readonly string[]): ServiceCapabilities | undefined {
  const signed = resources.filter(resource => resource.startsWith(prefix));
  if (!signed.length) return undefined;
  try {
    if (signed.length !== 1) throw new Error();
    const value: unknown = JSON.parse(decodeURIComponent(signed[0]!.slice(prefix.length)));
    if (!record(value) || Object.keys(value).some(key => !["vault", "phone"].includes(key))) throw new Error();
    if (value.vault !== undefined) {
      const vault = value.vault;
      if (!record(vault) || Object.keys(vault).some(key => !["ids", "origins", "request"].includes(key)) || !ids(vault.ids) || typeof vault.request !== "boolean"
        || !Array.isArray(vault.origins) || vault.origins.length > 64 || new Set(vault.origins).size !== vault.origins.length || !vault.origins.every(origin => {
          if (typeof origin !== "string") return false;
          const url = new URL(origin); return url.protocol === "https:" && url.origin === origin && !url.username && !url.password;
        })) throw new Error();
    }
    if (value.phone !== undefined) {
      const phone = value.phone;
      if (!record(phone) || Object.keys(phone).some(key => !["numberIds", "read", "provision", "release"].includes(key)) || !ids(phone.numberIds)
        || [phone.read, phone.provision, phone.release].some(flag => typeof flag !== "boolean")) throw new Error();
    }
    return value as ServiceCapabilities;
  } catch { throw new Error("The signed service permissions are invalid."); }
}

export function ServicePermissions({ services }: { services: ServiceCapabilities | undefined }) {
  if (!services) return null;
  return <div className="service-permissions">
    {services.vault ? <section aria-label="Vault permissions">
      <h3>Vault</h3>
      <p>{services.vault.request ? "Use these saved credentials in requests to the approved websites. Credential values remain private." : "View metadata for these saved credentials."}</p>
      <p>Vault items: {services.vault.ids.length ? services.vault.ids.join(", ") : "None"}</p>
      <p>Websites: {services.vault.origins.length ? services.vault.origins.join(", ") : "None"}</p>
    </section> : null}
    {services.phone ? <section aria-label="Phone permissions">
      <h3>Phone numbers</h3>
      <p>Numbers: {services.phone.numberIds.length ? services.phone.numberIds.join(", ") : "None selected"}</p>
      {services.phone.read ? <p>Read incoming messages, including verification codes, for these numbers.</p> : null}
      {services.phone.provision ? <p>Request a new number. You must separately approve the quoted recurring costs.</p> : null}
      {services.phone.release ? <p>Request release of these numbers. You must separately confirm release; linked accounts may lose verification access.</p> : null}
    </section> : null}
  </div>;
}
