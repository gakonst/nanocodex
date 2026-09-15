import assert from "node:assert/strict";
import test from "node:test";

import {
  isUserDataMutation,
  parseUserDataOperation,
  userDataToolInputSchema,
} from "../dist/user-data.js";

test("user data operations normalize pagination and time-series defaults", () => {
  assert.deepEqual(parseUserDataOperation({ operation: "document_list" }), {
    operation: "document_list",
    limit: 100,
  });
  const write = parseUserDataOperation({
    operation: "timeseries_write",
    series: "whoop.heart_rate_bpm",
    points: [{ timestamp_ms: 1_700_000_000_000, value: 72, fields: { source: "strap" } }],
  });
  assert.equal(write.conflict, "error");
  assert.equal(isUserDataMutation(write), true);
  assert.equal(isUserDataMutation(parseUserDataOperation({
    operation: "timeseries_query",
    series: "whoop.heart_rate_bpm",
  })), false);
});

test("user data rejects path ambiguity, non-finite values, and unknown fields", () => {
  assert.throws(
    () => parseUserDataOperation({ operation: "document_get", key: "../secret" }),
    /bounded relative identifier/,
  );
  assert.throws(
    () => parseUserDataOperation({
      operation: "timeseries_write",
      series: "whoop.hr",
      points: [{ timestamp_ms: 1, value: Number.NaN }],
    }),
    /finite numbers/,
  );
  assert.throws(
    () => parseUserDataOperation({ operation: "object_list", secret: true }),
    /unsupported field/,
  );
  assert.throws(
    () => parseUserDataOperation({ operation: "document_put", key: "fixture", value: { score: Infinity } }),
    /finite, acyclic JSON/,
  );
  assert.throws(
    () => parseUserDataOperation({ operation: "object_put", key: "fixture", content: "ok",
      encoding: "utf8", content_type: "text/plain", metadata: { missing: undefined } }),
    /finite, acyclic JSON/,
  );
});

test("user data schema keeps every operation closed", () => {
  const schema = userDataToolInputSchema();
  assert.deepEqual(schema.oneOf.map((operation) => operation.additionalProperties),
    Array(schema.oneOf.length).fill(false));
  assert.equal(schema.oneOf.length, 12);
});
