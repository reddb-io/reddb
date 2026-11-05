export type SecretHideReason =
  "control" | "timeout" | "unavailable" | "value-change" | "visibility";
/** Explicit presentation of a server-authorized value; not an authorization boundary. */
export interface SecretValueContract {
  /** Visible name of the sensitive value. */
  label: string;
  /** Plaintext only after application-authorized retrieval; omit while masked. */
  value?: string | null;
  /** Permission to offer reveal; defaults to false and never checks authorization itself. */
  revealable?: boolean;
  /** Caller-approved exposure; defaults to false. */
  revealed?: boolean;
  /** Application-owned retrieval in progress. */
  pending?: boolean;
  /** Caller-safe retrieval error, never inferred from exception text. */
  error?: string;
  /** Requests exposure or reports hiding; applications should drop plaintext when hiding. */
  onrevealedchange?: (revealed: boolean, reason: SecretHideReason) => void;
  /** Exposure lifetime in milliseconds; defaults to 30000; zero explicitly disables expiry. */
  hideAfter?: number;
  /** Masked-state text, independent of the secret length. */
  hiddenLabel?: string;
  /** Reveal action label. */
  revealLabel?: string;
  /** Hide action label. */
  hideLabel?: string;
}
