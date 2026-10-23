/** What a consumer injects to route the links inside a scope. */
export interface NavigationSeam {
  /** Activate an in-app destination instead of letting the browser follow it. */
  navigate: (href: string, event: MouseEvent) => void;
  /**
   * Turn a destination into the href a link renders. The Design System never
   * builds a route itself; this is for the caller composing links in the scope.
   */
  resolve?: (to: string) => string;
  /**
   * Whether this scope routes `href`. The default takes same-origin paths and
   * leaves fragments, other origins and non-HTTP schemes to the browser.
   */
  owns?: (href: string, anchor: HTMLAnchorElement) => boolean;
}

/** The default `owns`: a same-origin, non-fragment destination. */
export function ownsSameOrigin(href: string, anchor: HTMLAnchorElement): boolean {
  if (href === "" || href.startsWith("#")) return false;
  const url = new URL(anchor.href, anchor.ownerDocument.baseURI);
  if (url.protocol !== "http:" && url.protocol !== "https:") return false;
  return url.origin === new URL(anchor.ownerDocument.baseURI).origin;
}

/**
 * The anchor a click activates, when the scope should route it — or `null`
 * when the browser should keep the activation: a modified or non-primary
 * click (open in a new tab is the user's call), an already-handled event, a
 * link that targets another browsing context, a download, or an explicit
 * `data-native-navigation` opt-out.
 */
export function routableAnchor(
  event: MouseEvent,
  scope: Element,
  owns: NonNullable<NavigationSeam["owns"]> = ownsSameOrigin,
): HTMLAnchorElement | null {
  if (event.defaultPrevented || event.button !== 0) return null;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return null;
  const target = event.target;
  if (!(target instanceof Element)) return null;
  const anchor = target.closest<HTMLAnchorElement>("a[href]");
  if (!anchor || !scope.contains(anchor)) return null;
  if (anchor.hasAttribute("download") || anchor.hasAttribute("data-native-navigation")) return null;
  const targetContext = anchor.getAttribute("target");
  if (targetContext !== null && targetContext !== "" && targetContext !== "_self") return null;
  const href = anchor.getAttribute("href") ?? "";
  return owns(href, anchor) ? anchor : null;
}
