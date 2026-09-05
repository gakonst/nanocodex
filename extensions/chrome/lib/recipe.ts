export const RECIPE_SCHEMA_VERSION = 1 as const;
export const MAX_RECIPE_CSS_BYTES = 32 * 1024;
export const MAX_HIDE_SELECTORS = 64;

export interface SiteRecipe {
  schema_version: typeof RECIPE_SCHEMA_VERSION;
  name: string;
  css: string;
  hide_selectors: string[];
}

export interface StoredSiteRecipe {
  origin: string;
  recipe: SiteRecipe;
  updated_at_ms: number;
}

const UNSAFE_CSS = /(?:@|url\s*\(|image-set\s*\(|expression\s*\(|(?:https?|data|blob|javascript)\s*:|behavior\s*:|-moz-binding\s*:)/i;

export function validateRecipe(value: unknown): SiteRecipe {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("recipe must be an object");
  }
  const record = value as Record<string, unknown>;
  if (record.schema_version !== undefined && record.schema_version !== RECIPE_SCHEMA_VERSION) {
    throw new Error("unsupported recipe schema version");
  }
  if (typeof record.name !== "string" || !record.name.trim() || record.name.length > 80) {
    throw new Error("recipe name must contain 1 to 80 characters");
  }
  if (typeof record.css !== "string") throw new Error("recipe css must be a string");
  if (new TextEncoder().encode(record.css).byteLength > MAX_RECIPE_CSS_BYTES) {
    throw new Error("recipe css exceeds 32 KiB");
  }
  if (UNSAFE_CSS.test(record.css)) {
    throw new Error("recipe css cannot contain at-rules, network URLs, or executable CSS");
  }
  if (!Array.isArray(record.hide_selectors) || record.hide_selectors.length > MAX_HIDE_SELECTORS) {
    throw new Error(`recipe hide_selectors must contain at most ${MAX_HIDE_SELECTORS} selectors`);
  }
  const hideSelectors = record.hide_selectors.map((selector) => {
    if (typeof selector !== "string" || !selector.trim() || selector.length > 512) {
      throw new Error("each hidden selector must contain 1 to 512 characters");
    }
    if (selector.includes("\0") || selector.includes("{")) {
      throw new Error("hidden selector contains invalid characters");
    }
    return selector.trim();
  });
  return {
    schema_version: RECIPE_SCHEMA_VERSION,
    name: record.name.trim(),
    css: record.css,
    hide_selectors: hideSelectors
  };
}

export function compileRecipeCss(recipe: SiteRecipe): string {
  const hidden = recipe.hide_selectors.length === 0
    ? ""
    : `\n${recipe.hide_selectors.join(",\n")} { display: none !important; }`;
  return `${recipe.css}${hidden}`;
}

export function recipeStorageKey(origin: string): string {
  return `site-recipe:${normalizeOrigin(origin)}`;
}

export function normalizeOrigin(value: string): string {
  const url = new URL(value);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(
      "This tab cannot be changed. Open an HTTP or HTTPS page, then click the Nanocodex toolbar icon there.",
    );
  }
  return url.origin;
}

export function permissionPattern(origin: string): string {
  const url = new URL(normalizeOrigin(origin));
  return `${url.protocol}//${url.hostname}/*`;
}
