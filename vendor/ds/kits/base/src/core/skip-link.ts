/** Focus only a valid in-page target and respect consumer cancellation. */
export function focusSkipTarget(event: { defaultPrevented: boolean }, href: string, document: { getElementById(id: string): { focus(): void } | null }): void {
  if (event.defaultPrevented || !href.startsWith("#")) return;
  let id: string;
  try { id = decodeURIComponent(href.slice(1)); } catch { return; }
  document.getElementById(id)?.focus();
}
