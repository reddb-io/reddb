/** An OTP always has a finite positive integer number of native segments. */
export function codeLength(length: number): number {
  if (!Number.isSafeInteger(length) || length < 1) throw new Error("OneTimeCodeInput: length must be a positive integer.");
  return length;
}
/** Canonical ASCII alphanumeric entry; the caller's native pattern owns validation. */
export function codeCharacters(text: string, limit: number): string[] {
  return text.replace(/[^a-zA-Z0-9]/g, "").slice(0, limit).split("");
}
/** Keep empty positions while editing; flatten only for the public complete-code value. */
export function codeSegments(value: string, length: number): string[] {
  return Array.from({ length: codeLength(length) }, (_, index) => value[index] ?? "");
}

/** Native keyboard hint shared by both OTP adapters. */
export const ONE_TIME_CODE_INPUT_MODES = ["none", "text", "decimal", "numeric", "tel", "search", "email", "url"] as const;
export type OneTimeCodeInputMode = (typeof ONE_TIME_CODE_INPUT_MODES)[number];
