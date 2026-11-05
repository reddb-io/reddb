/** Native dialog lifecycle shared by every renderer, including nested scroll locks. */
const locks = new WeakMap<Document, { count: number; overflow: string }>();

/** A host shell must yield its shortcuts to events originating in a modal, including shadow DOM. */
export function isEventFromModal(event: Event): boolean {
  const path = event.composedPath();
  const targets = path.length ? path : [event.target];
  return targets.some(target => {
    const element = target as Element | null;
    return element?.nodeType === 1 && typeof element.closest === "function" &&
      element.closest('dialog[open], [role="dialog"][aria-modal="true"], [role="alertdialog"][aria-modal="true"]') !== null;
  });
}
export const modalFocusable = 'button:not([disabled]),a[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';
export interface ModalOptions {
  initialFocus(): "body" | "actions";
  dismissible(): boolean;
  requestOpen(open: boolean): void;
}
export function createModalController(dialog: HTMLDialogElement, options: ModalOptions) {
  const document = dialog.ownerDocument;
  let active = false, returnFocus: HTMLElement | null = null;
  const focusables = () => [...dialog.querySelectorAll<HTMLElement>(modalFocusable)].filter(element => !element.closest('[hidden],[inert]') && !element.matches(":disabled") && element.tabIndex >= 0 && element.closest("dialog") === dialog);
  function release(close = false) {
    if (!active) return;
    active = false;
    const lock = locks.get(document);
    if (lock && --lock.count === 0) { document.documentElement.style.overflow = lock.overflow; locks.delete(document); }
    if (close && dialog.open) { if (typeof dialog.close === "function") dialog.close(); else dialog.removeAttribute("open"); }
    if (returnFocus?.isConnected) returnFocus.focus();
    returnFocus = null;
  }
  function sync(open: boolean) {
    if (open && !active) {
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      if (!dialog.open) {
        if (typeof dialog.showModal === "function") dialog.showModal(); else dialog.setAttribute("open", "");
      }
      active = true;
      const lock = locks.get(document) ?? { count: 0, overflow: document.documentElement.style.overflow };
      lock.count++; locks.set(document, lock); document.documentElement.style.overflow = "hidden";
      const region = dialog.querySelector<HTMLElement>(options.initialFocus() === "actions" ? "[data-dialog-actions]" : "[data-dialog-body]");
      (focusables().find(element => region?.contains(element)) ?? focusables()[0] ?? dialog).focus();
    } else if (!open && active) {
      release(true);
    }
  }
  function keydown(event: KeyboardEvent) {
    if (event.target instanceof Element && event.target.closest("dialog") !== dialog) return;
    if (event.key !== "Tab") return;
    if (event.target instanceof Element && event.target.closest("[data-ds-focus-scope]")) return;
    const nodes = focusables(), first = nodes[0], last = nodes.at(-1);
    if (!first || !last) { event.preventDefault(); dialog.focus(); }
    else if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }
  // Managed popups arbitrate Escape on Document. Listen after them at Window
  // so one key dismisses only the innermost active layer, including portals.
  function escape(event: KeyboardEvent) {
    if (!active || event.defaultPrevented || event.key !== "Escape" || !(event.target instanceof Element) || event.target.closest("dialog") !== dialog) return;
    event.preventDefault(); event.stopPropagation(); options.requestOpen(false);
  }
  function click(event: MouseEvent) {
    if (!options.dismissible() || event.target !== dialog) return;
    const box = dialog.getBoundingClientRect();
    if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) options.requestOpen(false);
  }
  function cancel(event: Event) { event.preventDefault(); options.requestOpen(false); }
  function closed() { if (active && !dialog.open) { release(); options.requestOpen(false); } }
  document.defaultView?.addEventListener("keydown", escape);
  dialog.addEventListener("keydown", keydown); dialog.addEventListener("click", click); dialog.addEventListener("cancel", cancel); dialog.addEventListener("close", closed);
  return { sync, destroy() { release(true); document.defaultView?.removeEventListener("keydown", escape); dialog.removeEventListener("keydown", keydown); dialog.removeEventListener("click", click); dialog.removeEventListener("cancel", cancel); dialog.removeEventListener("close", closed); } };
}
