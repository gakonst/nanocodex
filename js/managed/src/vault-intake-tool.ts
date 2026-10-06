import type { NamedTool, ToolContext } from "nanocodex";

/** Requests a client-owned form. This tool never accepts or stores credential values. */
export function createVaultIntakeTool(authorize: (context: ToolContext) => void): NamedTool {
  return {
    name: "request_vault_intake",
    description: "Show an inline secure Vault form to the authenticated user. Use when the user asks to add credentials to their Vault. Supports TOTP enrollment from a seed or otpauth URI in the private form; never ask for or pass credentials, seeds, URIs or generated codes in chat or tools. The user submits directly to Vault; input_required is not confirmation of storage. Wait for the saved receipt before using the item. Saved items are available for the user’s authorized tasks without another website-approval prompt. An optional login origin is an exact HTTPS website hint, not a permission grant.",
    parameters: {
      type: "object", additionalProperties: false, required: ["kind"],
      properties: {
        operation: { type: "string", enum: ["create"] },
        kind: { type: "string", enum: ["login", "api_key", "card", "address", "phone", "totp"] },
        name: { type: "string", minLength: 1, maxLength: 120, description: "Suggested non-secret item label." },
        origin: { type: "string", maxLength: 2048, description: "Optional HTTPS website hint for a login, without path, query, fragment or credentials." },
      },
    },
    handler: (input, context) => {
      authorize(context);
      if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("Invalid Vault intake request");
      const value = input as Record<string, unknown>;
      if (Object.keys(value).some(key => !["kind", "name", "origin", "operation", "vault_id"].includes(key))
        || !["login", "api_key", "card", "address", "phone", "totp"].includes(String(value.kind))
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
      // Retained tool calls from older turns must not recreate the retired gate.
      if (operation === "authorize_origin") return {
        type: "vault_intake", status: "not_required", operation, kind: "login",
        message: "Saved logins can be used directly for authorized tasks. Open the private browser with the saved login and task destination.",
      };
      return { type: "vault_intake", status: "input_required", operation, kind: value.kind,
        ...(value.name === undefined ? {} : { name: value.name }),
        ...(value.origin === undefined ? {} : { origin: value.origin }),
      };
    },
  };
}
