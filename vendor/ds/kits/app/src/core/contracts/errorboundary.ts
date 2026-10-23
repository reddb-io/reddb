/** Render-recovery presentation shared by native client error boundaries. */
export interface ErrorBoundaryContract {
  /** Safe visible fallback title; raw exception text is never displayed by default. */
  title?: string;
  /** Caller-safe explanation of the failed region. */
  description?: string;
  /** Explicit retry action label. */
  retryLabel?: string;
  /** Changing identity remounts the region and clears its captured render failure. */
  resetKey?: string | number;
  /** Observes native client render/lifecycle failures; logging remains application-owned. */
  onfailure?: (error: unknown) => void;
  /** Called before retrying the region; the application can repair the failing state. */
  onretry?: () => void;
}
