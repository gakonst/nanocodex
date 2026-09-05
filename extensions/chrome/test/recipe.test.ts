import assert from "node:assert/strict";
import test from "node:test";
import {
  compileRecipeCss,
  normalizeOrigin,
  permissionPattern,
  recipeStorageKey,
  validateRecipe,
} from "../lib/recipe.ts";

test("validates and compiles the declarative recipe contract", () => {
  const recipe = validateRecipe({
    name: "Quiet reading",
    css: "article { max-width: 70ch; margin-inline: auto; }",
    hide_selectors: ["aside.ads", "#newsletter"],
  });
  assert.equal(recipe.schema_version, 1);
  assert.match(compileRecipeCss(recipe), /aside\.ads,\n#newsletter \{ display: none !important; \}/);
});

test("rejects CSS that can fetch code or data", () => {
  for (const css of [
    "@import 'https://bad.test/x.css';",
    ".x { background: url(https://bad.test/pixel); }",
    ".x { background: image-set(\"https://bad.test/pixel\" 1x); }",
    ".x { cursor: https://bad.test/pixel; }",
    ".x { width: expression(alert(1)); }",
    ".x { behavior: url(x.htc); }",
  ]) {
    assert.throws(() => validateRecipe({ name: "bad", css, hide_selectors: [] }), /cannot contain/);
  }
});

test("scopes storage exactly by origin while requesting Chrome's narrow host pattern", () => {
  assert.equal(normalizeOrigin("https://example.test/private?q=secret"), "https://example.test");
  assert.equal(recipeStorageKey("https://example.test/a"), "site-recipe:https://example.test");
  assert.equal(permissionPattern("https://example.test/a"), "https://example.test/*");
  assert.throws(
    () => normalizeOrigin("chrome://settings"),
    /Open an HTTP or HTTPS page, then click the Nanocodex toolbar icon there/,
  );
});
