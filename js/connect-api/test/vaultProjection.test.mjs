import assert from "node:assert/strict";
import test from "node:test";

import { projectVaultEntries } from "../src/vaultProjection.mjs";

const id = (character) => character.repeat(32);

test("Vault metadata projection reconstructs the exact safe per-kind account-info view", () => {
  const projected = projectVaultEntries([
    {
      id: id("a"), kind: "login", name: "Example", created_at: 1,
      username: "person@example.test", password: "never", unknown: "never",
    },
    {
      id: id("b"), kind: "card", name: "Work card", created_at: 2,
      last4: "4242", card_number: "4242424242424242", expiry_month: "01",
      expiry_year: "31", cvv: "123", billing_zip: "10001",
    },
    {
      id: id("c"), kind: "address", name: "Home", created_at: 3,
      address_line_1: "1 Private Way", address_line_2: "Unit 2", city: "Athens",
      state: "Attica", zip: "10558", country: "GR", password: "never",
    },
    {
      id: id("d"), kind: "phone", name: "Mobile", created_at: 4,
      phone_number: "+30 690 000 0000", secret: "never",
    },
  ]);

  assert.deepEqual(projected, [
    {
      id: id("a"), kind: "login", name: "Example", created_at: 1,
      username: "person@example.test",
    },
    { id: id("b"), kind: "card", name: "Work card", created_at: 2, last4: "4242" },
    {
      id: id("c"), kind: "address", name: "Home", created_at: 3,
      address_line_1: "1 Private Way", address_line_2: "Unit 2", city: "Athens",
      state: "Attica", zip: "10558", country: "GR",
    },
    {
      id: id("d"), kind: "phone", name: "Mobile", created_at: 4,
      phone_number: "+30 690 000 0000",
    },
  ]);
  const encoded = JSON.stringify(projected);
  for (const forbidden of [
    "never", "card_number", "expiry_month", "expiry_year", "cvv", "billing_zip", "unknown",
  ]) assert.equal(encoded.includes(forbidden), false, forbidden);
});

test("Vault metadata projection fails closed on malformed or unbounded snapshots", () => {
  const valid = { id: id("a"), kind: "card", name: "Card", created_at: 1, last4: "4242" };
  const invalid = [
    undefined,
    {},
    Array.from({ length: 101 }, () => valid),
    [{ ...valid, id: "short" }],
    [valid, valid],
    [{ ...valid, kind: "password" }],
    [{ ...valid, last4: "42x2" }],
    [{ ...valid, name: "x".repeat(121) }],
    [{ ...valid, created_at: -1 }],
  ];
  for (const value of invalid) assert.throws(() => projectVaultEntries(value));
});
