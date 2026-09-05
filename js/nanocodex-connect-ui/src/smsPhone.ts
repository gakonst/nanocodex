const E164_DIGITS = /^[1-9]\d{7,14}$/;

/** Normalizes an international mobile number without guessing a local country. */
export function normalizeSmsPhone(value: string): string | undefined {
  const compact = value.trim().replace(/[\s().-]/g, "");
  if (compact.startsWith("+")) {
    return E164_DIGITS.test(compact.slice(1)) ? compact : undefined;
  }
  if (compact.startsWith("00")) {
    const digits = compact.slice(2);
    return E164_DIGITS.test(digits) ? `+${digits}` : undefined;
  }
  // The North American country code plus its ten-digit national number is
  // unambiguous. Other digit-only formats still need an explicit + or 00.
  return /^1\d{10}$/.test(compact)
    ? `+${compact}`
    : undefined;
}
