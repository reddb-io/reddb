/** Development detection works in Vite and native React build pipelines. */
function a11yDevelopment(): boolean {
  return (import.meta as { env?: { DEV?: boolean } }).env?.DEV ??
    (globalThis as { process?: { env?: { NODE_ENV?: string } } }).process?.env?.NODE_ENV === "development";
}

// Framework-independent accessible-name diagnostics. Never changes DOM semantics.

const warned = new Set<string>();

/**
 * Warn, once per session and only in development, that `component` broke an
 * accessibility contract. `problem` says what is missing, `fix` the props that
 * supply it. The sibling of `warnDeprecated`, for the same audience.
 */
export function warnA11y(component: string, problem: string, fix: string): void {
  if (!a11yDevelopment()) return;
  const key = `${component}\u0000${problem}`;
  if (warned.has(key)) return;
  warned.add(key);
  console.warn(`[reddb ${component}] ${problem}. ${fix}`);
}

function nonEmpty(value: string | null | undefined): boolean {
  return typeof value === "string" && value.trim() !== "";
}

/** Text a subtree contributes to a name, skipping what is hidden from assistive technology. */
function namedContent(node: Node): boolean {
  if (node.nodeType === 3) return nonEmpty(node.textContent);
  if (node.nodeType !== 1) return false;
  const element = node as Element;
  if (element.getAttribute("aria-hidden") === "true") return false;
  if (nonEmpty(element.getAttribute("aria-label"))) return true;
  if (element.tagName === "IMG" && nonEmpty(element.getAttribute("alt"))) return true;
  if (element.tagName.toLowerCase() === "title") return nonEmpty(element.textContent);
  for (const child of element.childNodes) if (namedContent(child)) return true;
  return false;
}

/**
 * Whether `element` has an accessible name a screen reader can announce:
 * `aria-label`, an `aria-labelledby` that resolves to text, a native `<label>`
 * (wrapping or `for`), a `title`, or — for a button — its own content.
 *
 * A pragmatic approximation of the accessible-name computation, used only to
 * decide whether to warn in development, never to change what renders.
 */
export function hasAccessibleName(element: Element, { fromContent = false } = {}): boolean {
  if (nonEmpty(element.getAttribute("aria-label"))) return true;
  const labelledBy = element.getAttribute("aria-labelledby");
  if (nonEmpty(labelledBy)) {
    const root = element.getRootNode() as Document | ShadowRoot;
    for (const id of labelledBy!.trim().split(/\s+/)) {
      const target = root.getElementById?.(id) ?? element.ownerDocument.getElementById(id);
      if (target && nonEmpty(target.textContent)) return true;
    }
  }
  const labels = (element as HTMLInputElement).labels;
  if (labels) for (const label of labels) if (nonEmpty(label.textContent)) return true;
  if (nonEmpty(element.getAttribute("title"))) return true;
  return fromContent && namedContent(element);
}

/** The ways a field gets its name, quoted in every field warning. */
export const FIELD_NAME_FIX =
  "Wrap it in a Field with a `label`, or pass `aria-label` or `aria-labelledby`, or point a native <label for> at its id";

/**
 * Warn in development when a mounted form control has no accessible name.
 * Checked after mount, so a `<label for>` written anywhere in the document
 * counts, exactly as it does for the browser.
 */
export function checkFieldName(component: string, element: Element | undefined): void {
  if (!a11yDevelopment() || !element) return;
  if (hasAccessibleName(element)) return;
  warnA11y(component, "renders a form control with no accessible name", `${FIELD_NAME_FIX}.`);
}

/** Visible words for Label in Name; screen-reader-only content and decorative SVG are excluded. */
function contentWords(node: Node): string {
  if (node.nodeType === 3) return node.textContent ?? "";
  if (node.nodeType !== 1) return "";
  const element = node as Element;
  if (element.matches('[hidden], .hidden, .sr-only, svg, script, style')) return "";
  return [...element.childNodes].map(contentWords).join(" ");
}

/** Buttons keep their visible words in the accessible name (WCAG 2.5.3). */
export function checkButtonName(component: string, element: Element | undefined): void {
  if (!a11yDevelopment() || !element) return;
  if (!hasAccessibleName(element, { fromContent: true })) {
    warnA11y(component, "renders with no text and no `aria-label`, so it has no accessible name", "Pass `aria-label` (or `aria-labelledby`) to an icon-only Button.");
    return;
  }
  const normalize = (text: string) => text.replace(/\s+/g, " ").trim().toLocaleLowerCase();
  const visible = normalize(contentWords(element));
  if (!/[\p{L}\p{N}]/u.test(visible)) return;
  const ids = element.getAttribute("aria-labelledby")?.trim().split(/\s+/).filter(Boolean) ?? [];
  const root = element.getRootNode() as Document | ShadowRoot;
  const referenced = ids.map(id => (root.getElementById?.(id) ?? element.ownerDocument.getElementById(id))?.textContent ?? "").join(" ");
  const name = normalize(referenced || element.getAttribute("aria-label") || visible);
  if (!name.includes(visible)) {
    warnA11y(component, "visible text is missing from its accessible name", "Keep the visible action in the name; use `aria-describedby` for row context instead of replacing the action with `aria-labelledby`.");
  }
}
