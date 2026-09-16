type ChatGptAccount = Readonly<{
  accountId: string;
  connected: boolean;
  active: boolean;
  limitedUntil?: number;
}>;

export type CredentialStatus = Readonly<{
  ready: boolean;
  active: "openai" | "chatgpt" | null;
  openai: { connected: boolean };
  chatgpt: {
    connected: boolean;
    accountId?: string;
    accounts: readonly ChatGptAccount[];
    login?: {
      verificationUrl: string;
      userCode: string;
      expiresAt: number;
      pollAfterMs: number;
    };
  };
}>;

export function decodeCredentialStatus(value: unknown): CredentialStatus {
  if (!isRecord(value) || !isRecord(value.openai) || !isRecord(value.chatgpt)) {
    throw new Error("Invalid model connection response.");
  }
  const active = value.active === "openai" || value.active === "chatgpt" ? value.active : null;
  if (typeof value.ready !== "boolean"
    || typeof value.openai.connected !== "boolean"
    || typeof value.chatgpt.connected !== "boolean") {
    throw new Error("Invalid model connection response.");
  }
  const login = value.chatgpt.login === undefined
    ? undefined
    : decodeChatGptLogin(value.chatgpt.login);
  return {
    ready: value.ready,
    active,
    openai: { connected: value.openai.connected },
    chatgpt: {
      connected: value.chatgpt.connected,
      accounts: decodeChatGptAccounts(value.chatgpt, active),
      ...(typeof value.chatgpt.account_id === "string" ? { accountId: value.chatgpt.account_id } : {}),
      ...(login ? { login } : {}),
    },
  };
}

export function decodeChatGptLogin(value: unknown): NonNullable<CredentialStatus["chatgpt"]["login"]> {
  if (!isRecord(value)
    || value.state !== "pending"
    || typeof value.verification_url !== "string"
    || typeof value.user_code !== "string"
    || typeof value.expires_at !== "number"
    || typeof value.poll_after_ms !== "number") {
    throw new Error("Invalid ChatGPT sign-in response.");
  }
  return {
    verificationUrl: value.verification_url,
    userCode: value.user_code,
    expiresAt: value.expires_at,
    pollAfterMs: value.poll_after_ms,
  };
}

function decodeChatGptAccounts(value: Record<string, unknown>, active: CredentialStatus["active"]): ChatGptAccount[] {
  // Keep older deployments visible while the account pool rolls out.
  if (value.accounts === undefined) {
    return typeof value.account_id === "string" ? [{
      accountId: value.account_id, connected: value.connected === true, active: active === "chatgpt",
    }] : [];
  }
  if (!Array.isArray(value.accounts)) throw new Error("Invalid ChatGPT account list.");
  return value.accounts.map((account) => {
    if (!isRecord(account) || typeof account.account_id !== "string" || !account.account_id
      || typeof account.connected !== "boolean" || typeof account.active !== "boolean"
      || (account.limited_until !== undefined && (typeof account.limited_until !== "number"
        || !Number.isFinite(account.limited_until)))) {
      throw new Error("Invalid ChatGPT account list.");
    }
    return {
      accountId: account.account_id, connected: account.connected, active: account.active,
      ...(typeof account.limited_until === "number" ? { limitedUntil: account.limited_until } : {}),
    };
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
