const subjects = new WeakMap();
const BROKER_SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;

/** @internal Scopes a shared Service Binding to one opaque Durable Object subject. */
export function scopeCloudflareEgress(binding, subject) {
  if (typeof subject !== "string" || !BROKER_SUBJECT.test(subject)) {
    throw new TypeError("Cloudflare Agent requires an opaque Durable Object identity");
  }
  const scoped = Object.freeze({
    fetch: (input, init) => binding.fetch(input, init),
  });
  subjects.set(scoped, subject);
  return scoped;
}

/** @internal Returns only subjects attached by the Cloudflare Agent adapter. */
export function cloudflareEgressSubject(binding) {
  return subjects.get(binding);
}
