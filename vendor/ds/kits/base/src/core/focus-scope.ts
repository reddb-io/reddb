import { modalFocusable } from "./modal";
/** Keep boundary Tab moves inside a popup mounted in a native modal's top layer.
 * Native dialogs can intercept the headless engine's off-surface focus guards.
 * The innermost scope alone owns this boundary; ordinary moves stay native.
 */
export function trapScopeTab(scope: HTMLElement): () => void {
  const keydown = (event: KeyboardEvent) => {
    if (event.defaultPrevented || event.key !== "Tab" || !(event.target instanceof Element) || event.target.closest("[data-ds-focus-scope]") !== scope) return;
    const nodes = [...scope.querySelectorAll<HTMLElement>(modalFocusable)].filter(element => !element.closest("[hidden],[inert]") && !element.matches(":disabled") && element.tabIndex >= 0 && getComputedStyle(element).display !== "none");
    const first = nodes[0], last = nodes.at(-1);
    if (!first || !last) { event.preventDefault(); scope.focus(); }
    else if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  };
  scope.addEventListener("keydown", keydown, true);
  return () => scope.removeEventListener("keydown", keydown, true);
}

/** A consumer may deliberately focus its next field while a popup closes.
 * Restore the trigger only when focus still belongs to the popup or was lost.
 */
export function focusMovedOutside(anchor: HTMLElement | undefined, popupSelector: string): boolean {
  const document = anchor?.ownerDocument;
  const active = document?.activeElement;
  return !!active && active !== document?.body && active !== document?.documentElement &&
    !anchor?.contains(active) && !active.closest(popupSelector);
}
