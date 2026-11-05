/** Clamp caller-owned remaining time to whole, non-negative seconds. */
export function normalizeRemaining(remaining: number): number {
  if (!Number.isFinite(remaining)) return 0;
  return Math.max(0, Math.floor(remaining));
}

/** A compact clock rendering that keeps hours when a duration crosses one hour. */
export function formatDuration(remaining: number): string {
  const seconds = normalizeRemaining(remaining);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  const pair = (value: number): string => String(value).padStart(2, "0");

  return hours > 0 ? `${hours}:${pair(minutes)}:${pair(rest)}` : `${minutes}:${pair(rest)}`;
}

/** The same normalized duration in the machine-readable form native `<time>` expects. */
export function durationAttribute(remaining: number): string {
  const seconds = normalizeRemaining(remaining);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;

  return `PT${hours > 0 ? `${hours}H` : ""}${minutes > 0 ? `${minutes}M` : ""}${rest > 0 || seconds === 0 ? `${rest}S` : ""}`;
}
