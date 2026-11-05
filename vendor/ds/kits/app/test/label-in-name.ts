// WCAG 2.5.3 (Label in Name) as axe-core 4.14 checks it in
// `label-content-name-mismatch`, reduced to what a DOM test can see. The
// visible text is the element's text less `.sr-only` content (aria-hidden text
// is still on screen, so it counts); both strings are cut into words, with
// parentheticals and punctuation dropped, and the visible words must run, in
// order, inside the accessible name. The axe gate proves the rule in a real
// browser; this lets a unit test pin a component's naming without it.

const words = (text: string): string[] =>
  text
    .normalize("NFKD")
    .replace(/\([^()]*\)/g, " ")
    .toLowerCase()
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean);

/** The text a sighted reader sees on `element`: its text content less `.sr-only` nodes. */
export function visibleText(element: Element): string {
  const clone = element.cloneNode(true) as Element;
  for (const hidden of clone.querySelectorAll(".sr-only")) hidden.remove();
  return (clone.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** The accessible name: `aria-labelledby`, then `aria-label`, then the element's text. */
export function accessibleName(element: Element): string {
  const labelledby = element.getAttribute("aria-labelledby");
  if (labelledby) {
    return labelledby
      .split(/\s+/)
      .map((id) => element.ownerDocument.getElementById(id)?.textContent ?? "")
      .join(" ")
      .trim();
  }
  return (element.getAttribute("aria-label") ?? element.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** Whether the visible words run, in order, inside the accessible name. */
export function labelInName(element: Element): boolean {
  const shown = words(visibleText(element));
  if (shown.length === 0) return true;
  const named = words(accessibleName(element));
  return named.some((_, start) => shown.every((word, offset) => named[start + offset] === word));
}
