// Bits UI's Collapsible measures its content height in the browser; during
// SSR the measurement does not exist yet, so the style it hands the content
// element carries `--bits-collapsible-content-height: undefined`. Serving the
// literal word "undefined" is a rendering defect (and the showcase's
// renders-cleanly contract rejects it), so the declarations without a real
// value are dropped before the spread reaches the DOM. The browser-side
// measurement re-applies them after hydration.
export function sanitizeCollapsibleStyle(style: unknown): string | undefined {
  if (typeof style !== "string" || style.length === 0) return undefined;
  const kept = style
    .split(";")
    .map((declaration) => declaration.trim())
    .filter((declaration) => declaration.length > 0 && !/:\s*undefined$/.test(declaration));
  return kept.length > 0 ? kept.join("; ") : undefined;
}
