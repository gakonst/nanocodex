import { useId, useState, type ReactNode } from "react";
import { normalizeSmsPhone } from "./smsPhone.js";

export type StoredPasskey = Readonly<{
  address: `0x${string}`;
  credentialId: string;
  current?: boolean | undefined;
  label?: string | undefined;
}>;

export type AccountSelection = Readonly<{
  address?: `0x${string}` | undefined;
  authentication?: "sms_otp" | undefined;
  current?: boolean | undefined;
  mode: "login" | "register";
  label: string;
  credentialId?: string | undefined;
  discoverCredential?: boolean | undefined;
}>;

type OtpChallenge = Readonly<{
  challengeId: string;
  expiresAt: number;
  phone: string;
}>;

export function AccountChooser({
  confirmationCode,
  description = "Sign in with the code sent to your phone.",
  disabled,
  failure,
  requestContext,
  onCancel,
  onChooseAccount,
  authOrigin = "",
}: Readonly<{
  authOrigin?: string | undefined;
  confirmationCode?: string | undefined;
  description?: string | undefined;
  disabled: boolean;
  failure?: string | null | undefined;
  requestContext?: ReactNode;
  newAccountDetail?: string | undefined;
  onCancel?: (() => void) | undefined;
  onChooseAccount(account: AccountSelection): void;
  storedPasskeys?: readonly StoredPasskey[] | undefined;
}>) {
  const phoneId = useId();
  const codeId = useId();
  const phoneHintId = useId();
  const codeHintId = useId();
  const failureId = useId();
  const [phone, setPhone] = useState("");
  const [code, setCode] = useState("");
  const [challenge, setChallenge] = useState<OtpChallenge>();
  const [operation, setOperation] = useState<"send" | "verify">();
  const [localFailure, setLocalFailure] = useState<string>();

  async function sendCode() {
    if (operation) return;
    const normalized = normalizeSmsPhone(phone);
    if (!normalized) {
      setLocalFailure(/^\d{6}$/.test(phone.trim())
        ? "That looks like a verification code. First enter your mobile number; we’ll ask for the code next."
        : "Enter a valid mobile number with its country code, like +30 697 123 4567.");
      return;
    }
    setPhone(normalized);
    setOperation("send");
    setLocalFailure(undefined);
    try {
      const response = await fetch(`${authOrigin}/v1/auth/sms/start`, {
        method: "POST",
        credentials: "include",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ phone: normalized }),
      });
      const body: unknown = await response.json().catch(() => undefined);
      if (!response.ok) throw new Error(otpError(body, "Couldn’t send the code."));
      if (!isRecord(body)
        || typeof body.challenge_id !== "string"
        || typeof body.expires_in !== "number") {
        throw new Error("The account service returned an invalid challenge.");
      }
      setChallenge({
        challengeId: body.challenge_id,
        expiresAt: Date.now() + body.expires_in * 1_000,
        phone: normalized,
      });
      setCode("");
    } catch (cause) {
      setLocalFailure(errorMessage(cause));
    } finally {
      setOperation(undefined);
    }
  }

  async function verifyCode() {
    if (!challenge || operation) return;
    if (!/^\d{6}$/.test(code)) {
      setLocalFailure("Enter the six-digit code from the message.");
      return;
    }
    setOperation("verify");
    setLocalFailure(undefined);
    try {
      const response = await fetch(`${authOrigin}/v1/auth/sms/verify`, {
        method: "POST",
        credentials: "include",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          challenge_id: challenge.challengeId,
          code,
          phone: challenge.phone,
        }),
      });
      const body: unknown = await response.json().catch(() => undefined);
      if (!response.ok) throw new Error(otpError(body, "That code didn’t work."));
      if (!isRecord(body) || !isRecord(body.user)
        || typeof body.user.id !== "string"
        || (body.user.address !== undefined
          && (typeof body.user.address !== "string"
            || !/^0x[0-9a-f]{40}$/.test(body.user.address)))) {
        throw new Error("The account service returned an invalid session.");
      }
      onChooseAccount({
        ...(body.user.address ? { address: body.user.address as `0x${string}` } : {}),
        authentication: "sms_otp",
        current: true,
        label: maskedPhone(challenge.phone),
        mode: "login",
      });
    } catch (cause) {
      setLocalFailure(errorMessage(cause));
    } finally {
      setOperation(undefined);
    }
  }

  const unavailable = disabled || operation !== undefined;
  const visibleFailure = localFailure ?? failure;
  return (
    <div className="wizard-page wizard-account-page">
      <section className="sms-auth-panel" aria-labelledby={`${phoneId}-heading`}>
        <header className="wizard-intro">
          <div className="wizard-app">
            <span>Nanocodex account · Step {challenge ? "2" : "1"} of 2</span>
            <h1 id={`${phoneId}-heading`}>{challenge ? "Enter your code" : "Enter your phone number"}</h1>
            <p>{challenge
              ? "Use the six-digit code from the text message to finish signing in."
              : description}</p>
          </div>
          {confirmationCode ? (
            <div className="wizard-terminal-code" role="status">
              <span>Terminal code</span>
              <strong>{confirmationCode.slice(0, 4)}-{confirmationCode.slice(4)}</strong>
            </div>
          ) : null}
        </header>

        {visibleFailure ? (
          <div className="account-failure" id={failureId} role="alert"><p>{visibleFailure}</p></div>
        ) : null}
        {requestContext ? <div className="wizard-sections">{requestContext}</div> : null}

        {!challenge ? (
          <form className="sms-otp-form" noValidate onSubmit={(event) => {
          event.preventDefault();
          void sendCode();
        }}>
          <label htmlFor={phoneId}>Mobile number</label>
          <p id={phoneHintId}>Include the country code. We’ll text you a one-time code.</p>
          <div className="sms-otp-input-row">
            <input
              aria-describedby={`${phoneHintId}${visibleFailure ? ` ${failureId}` : ""}`}
              aria-invalid={localFailure ? true : undefined}
              autoComplete="tel"
              autoFocus
              disabled={unavailable}
              id={phoneId}
              inputMode="tel"
              onChange={(event) => {
                setPhone(event.target.value);
                setLocalFailure(undefined);
              }}
              placeholder="+30 697 123 4567"
              required
              type="tel"
              value={phone}
            />
            <button disabled={unavailable} type="submit">
              {operation === "send" ? "Sending…" : "Text me a code"}
            </button>
          </div>
          <p>By continuing, you agree to receive an automated one-time account code. Message and data rates may apply.</p>
        </form>
      ) : (
        <form className="sms-otp-form" noValidate onSubmit={(event) => {
          event.preventDefault();
          void verifyCode();
        }}>
          <label htmlFor={codeId}>6-digit code</label>
          <p id={codeHintId}>Sent to {maskedPhone(challenge.phone)}. It expires at {new Date(challenge.expiresAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}.</p>
          <div className="sms-otp-input-row">
            <input
              aria-describedby={`${codeHintId}${visibleFailure ? ` ${failureId}` : ""}`}
              aria-invalid={localFailure ? true : undefined}
              autoComplete="one-time-code"
              autoFocus
              disabled={unavailable}
              id={codeId}
              inputMode="numeric"
              maxLength={6}
              onChange={(event) => {
                setCode(event.target.value.replace(/\D/g, "").slice(0, 6));
                setLocalFailure(undefined);
              }}
              pattern="[0-9]{6}"
              placeholder="000000"
              required
              value={code}
            />
            <button disabled={unavailable} type="submit">
              {operation === "verify" ? "Checking…" : "Continue"}
            </button>
          </div>
          <button
            className="sms-otp-change"
            disabled={unavailable}
            onClick={() => {
              setChallenge(undefined);
              setCode("");
              setLocalFailure(undefined);
            }}
            type="button"
          >Use a different number</button>
        </form>
      )}
        <div className="sms-auth-security" aria-label="Security note">
          <span aria-hidden="true">✓</span>
          <p>Your wallet key stays encrypted in your account vault.</p>
        </div>
        {onCancel ? (
          <button className="wizard-cancel" disabled={unavailable} onClick={onCancel} type="button">Cancel</button>
        ) : null}
      </section>
    </div>
  );
}

export function orderedPasskeys(storedPasskeys: readonly StoredPasskey[]): readonly StoredPasskey[] {
  return storedPasskeys.some((account) => account.current)
    ? [...storedPasskeys].sort((left, right) => Number(right.current === true) - Number(left.current === true))
    : storedPasskeys;
}

function maskedPhone(value: string): string {
  const normalized = value.replace(/\D/g, "");
  return normalized.length > 4 ? `+••• ••${normalized.slice(-4)}` : "your phone";
}

function otpError(value: unknown, fallback: string): string {
  if (!isRecord(value) || typeof value.error !== "string") return fallback;
  if (value.error === "rate_limited") return "Too many codes requested. Wait a minute and try again.";
  if (value.error === "invalid_phone") return "Enter a mobile number with its country code.";
  if (value.error === "invalid_or_expired_otp") return "That code is invalid or expired.";
  if (value.error === "invalid_otp") return "Enter the six-digit code from the message.";
  if (value.error === "sms_delivery_failed") return "The code could not be delivered. Try again.";
  if (value.error === "sms_verification_failed") return "The code could not be checked right now. Try again.";
  if (value.error === "wallet_unavailable") return "Your account key could not be prepared. Try again.";
  if (value.error === "sms_otp_unavailable" || value.error === "sms_identity_unavailable") {
    return "Phone sign-in is temporarily unavailable. Try again.";
  }
  return fallback;
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : "The account service is unavailable.";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
