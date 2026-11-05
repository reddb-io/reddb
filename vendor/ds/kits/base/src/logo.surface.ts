// Which surface the Logo sits on when the caller does not say.
//
// ADR 0004's `on="light" | "dark"` names the surface behind the Mark. The
// Color Scheme, not the directional Theme, is the document-level signal that
// answers that contrast question. A local surface still uses `on` explicitly.

import { LOGO_SURFACES, type LogoSurface } from "./logo.marks";

/** The attribute the Color Scheme selectors key on. */
export const COLOR_SCHEME_ATTRIBUTE = "data-color-scheme";

/** The surface a Color Scheme implies. Anything unrecognised is light. */
export function surfaceOfColorScheme(scheme: string | null | undefined): LogoSurface {
  return scheme === "dark" ? "dark" : "light";
}

/** The surface implied by the active Color Scheme on `doc`. */
export function activeSurface(doc: Document | null | undefined = globalThis.document): LogoSurface {
  if (!doc) return "light";
  return surfaceOfColorScheme(doc.documentElement.getAttribute(COLOR_SCHEME_ATTRIBUTE));
}

/**
 * Observe root Color Scheme changes and report their implied Logo surface.
 * Returns a cleanup function; without a browser it reports once and is inert.
 */
export function observeActiveSurface(
  report: (surface: LogoSurface) => void,
  doc: Document | null | undefined = globalThis.document,
): () => void {
  report(activeSurface(doc));
  const Observer = doc?.defaultView?.MutationObserver;
  if (!doc || !Observer) return () => {};
  const observer = new Observer(() => report(activeSurface(doc)));
  observer.observe(doc.documentElement, {
    attributes: true,
    attributeFilter: [COLOR_SCHEME_ATTRIBUTE],
  });
  return () => observer.disconnect();
}

/** Is `value` a surface the Logo knows? */
export function isLogoSurface(value: unknown): value is LogoSurface {
  return typeof value === "string" && (LOGO_SURFACES as readonly string[]).includes(value);
}
