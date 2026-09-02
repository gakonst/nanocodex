import assert from "node:assert/strict";
import test from "node:test";

import {
  hostPrincipalExchangeFromResources,
  isHostPrincipal,
  sameHostPrincipal,
  sameOrderedResources,
} from "../src/hostPrincipal.mts";

const exchange = "e".repeat(43);
const resource = `urn:nanocodex:host-principal:exchange:${exchange}`;

test("host principal exchange is singular, opaque, and removed from grant resources", () => {
  assert.deepEqual(hostPrincipalExchangeFromResources(["agent", resource, "github"]), {
    exchange,
    resources: ["agent", "github"],
  });
  assert.equal(hostPrincipalExchangeFromResources(["agent"]), undefined);
  assert.throws(() => hostPrincipalExchangeFromResources([resource, resource]), /invalid/);
  assert.throws(() => hostPrincipalExchangeFromResources([
    "urn:nanocodex:host-principal:exchange:short",
  ]), /invalid/);
});

test("managed host principals are exact and retain the private session fence", () => {
  const principal = {
    kind: "host",
    id: "p".repeat(43),
    app_id: "acme",
    app_origin: "https://app.example",
    issuer: "https://identity.example/",
    tenant: "acme-production",
    session_epoch: 4,
    session_digest: "s".repeat(43),
  };
  assert.equal(isHostPrincipal(principal), true);
  assert.equal(isHostPrincipal({ ...principal, app_origin: "https://other.example" }), true);
  assert.equal(isHostPrincipal({ ...principal, session_epoch: -1 }), false);
  assert.equal(isHostPrincipal({ ...principal, session_epoch: 0 }), false);
  assert.equal(isHostPrincipal({ ...principal, issuer: "issuer\nclaim" }), false);
  assert.equal(isHostPrincipal({ ...principal, tenant: "" }), false);
  const { issuer: _issuer, ...missingIssuer } = principal;
  assert.equal(isHostPrincipal(missingIssuer), false);
  assert.equal(isHostPrincipal({ ...principal, provider_token: "secret" }), false);
  assert.equal(isHostPrincipal({ ...principal, app_origin: "http://app.example" }), false);
  assert.equal(sameHostPrincipal(principal, { ...principal }), true);
  assert.equal(sameHostPrincipal(principal, { ...principal, tenant: "acme-preview" }), false);
  assert.equal(sameHostPrincipal(principal, { ...principal, session_epoch: 5 }), false);
});

test("resource comparison is ordered, unique, and exact", () => {
  assert.equal(sameOrderedResources(["a", "b"], ["a", "b"]), true);
  assert.equal(sameOrderedResources(["a", "b"], ["b", "a"]), false);
  assert.equal(sameOrderedResources(["a", "a"], ["a", "a"]), false);
  assert.equal(sameOrderedResources(["a"], ["a", "b"]), false);
});
