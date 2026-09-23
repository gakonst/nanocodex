/** Cloudflare hints affect only first use and never change object identity.
 * Hints are best effort, not jurisdiction or execution-colo assertions.
 * https://developers.cloudflare.com/durable-objects/reference/data-location/ */
const TRUSTED_INGRESS_HEADER = "x-nanocodex-placement-colo";
const COLOS = {
  SFO: "wnam",
  SJC: "wnam",
  LAX: "wnam",
  SEA: "wnam",
  PDX: "wnam",
  PHX: "wnam",
  DEN: "wnam",
  LAS: "wnam",
  SLC: "wnam",
  IAD: "enam",
  EWR: "enam",
  BOS: "enam",
  ATL: "enam",
  ORD: "enam",
  MIA: "enam",
  LHR: "weur",
  CDG: "weur",
  FRA: "weur",
  AMS: "weur",
  MXP: "weur",
  MAD: "weur",
  DUB: "weur",
  ZRH: "weur",
  WAW: "eeur",
  OTP: "eeur",
  ATH: "eeur",
  SIN: "apac",
  NRT: "apac",
  HKG: "apac",
  SYD: "oc",
  MEL: "oc",
  AKL: "oc",
  GRU: "sam",
  SCL: "sam",
  EZE: "sam"
};
function ingressColo(value) {
  return typeof value === "string" && /^[A-Z]{3}$/.test(value) ? value : null;
}
function placementRegion(value) {
  const colo = ingressColo(value);
  return colo === null ? void 0 : COLOS[colo];
}
function durablePlacementOptions(colo) {
  const locationHint = placementRegion(colo);
  return locationHint ? { locationHint } : void 0;
}
/** Replace any caller assertion with platform or retained Session metadata. */
function placementHeaders(headers, colo) {
  const result = new Headers(headers);
  result.delete(TRUSTED_INGRESS_HEADER);
  const trusted = ingressColo(colo);
  if (trusted) result.set(TRUSTED_INGRESS_HEADER, trusted);
  return result;
}
/** Scope a request-local env only; never replace a retained DO env or its
 * discovery-cache/model-transport binding identities. */
function withIngressPlacement(env, colo) {
  const trustedClientIngressColo = ingressColo(colo);
  const binding = env.NANOCODEX;
  return { ...env, trustedClientIngressColo, ...binding ? {
    NANOCODEX: {
      fetch(input, init) {
        const request = new Request(input, init);
        return binding.fetch(new Request(request, { headers: placementHeaders(request.headers, trustedClientIngressColo) }));
      }
    }
  } : {} };
}
export {
  TRUSTED_INGRESS_HEADER,
  durablePlacementOptions,
  ingressColo,
  placementHeaders,
  placementRegion,
  withIngressPlacement
};
