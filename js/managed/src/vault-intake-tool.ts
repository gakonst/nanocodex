import type { NamedTool, ToolContext } from "nanocodex";

/** Requests a client-owned form. This tool never accepts or stores credential values. */
export function createVaultIntakeTool(authorize: (context: ToolContext) => void): NamedTool {
  return {
    name: "request_vault_intake",
    description: "Show an inline secure Vault form to the authenticated user. Use when the user asks to add credentials to their Vault. Never ask for or pass credential values in chat or tools. The user submits directly to Vault; input_required is not confirmation of storage. Wait for the saved receipt before using the item. Use operation authorize_origin with an existing opaque vault_id to request website approval without password reentry. A login origin must be an exact HTTPS origin.",
    parameters: {
      type: "object", additionalProperties: false, required: ["kind"],
      properties: {
        operation: { type: "string", enum: ["create", "authorize_origin"] },
        vault_id: { type: "string", pattern: "^[A-Za-z0-9_-]{22,64}$" },
        kind: { type: "string", enum: ["login", "api_key", "card", "address", "phone"] },
        name: { type: "string", minLength: 1, maxLength: 120, description: "Suggested non-secret item label." },
        origin: { type: "string", maxLength: 2048, description: "Exact HTTPS origin for a login, without path, query, fragment or credentials." },
      },
    },
    handler: (input, context) => {
      authorize(context);
      if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("Invalid Vault intake request");
      const value = input as Record<string, unknown>;
      if (Object.keys(value).some(key => !["kind", "name", "origin", "operation", "vault_id"].includes(key))
        || !["login", "api_key", "card", "address", "phone"].includes(String(value.kind))
        || (value.name !== undefined && (typeof value.name !== "string" || !value.name.trim() || value.name.length > 120 || /[\u0000-\u001f\u007f]/.test(value.name)))) {
        throw new TypeError("Invalid Vault intake request");
      }
      const operation = value.operation ?? "create";
      if (!["create", "authorize_origin"].includes(String(operation))
        || (operation === "create" && value.vault_id !== undefined)
        || (operation === "authorize_origin" && (value.kind !== "login" || typeof value.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(value.vault_id) || value.origin === undefined))) throw new TypeError("Invalid Vault intake request");
      if (value.origin !== undefined) {
        if (value.kind !== "login" || typeof value.origin !== "string" || value.origin.length > 2048) throw new TypeError("Invalid login origin");
        let url: URL;
        try { url = new URL(value.origin); } catch { throw new TypeError("Invalid login origin"); }
        if (url.protocol !== "https:" || url.origin !== value.origin || url.username || url.password) throw new TypeError("Invalid login origin");
      }
      return { type: "vault_intake", status: "input_required", operation, ...(operation === "authorize_origin" ? { vault_id: value.vault_id } : {}), kind: value.kind,
        ...(value.name === undefined ? {} : { name: value.name }),
        ...(value.origin === undefined ? {} : { origin: value.origin }),
      };
    },
  };
}
