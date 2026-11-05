import { createAttachmentKey } from "svelte/attachments";
import { observePortalAppearance, modalPortalContainer } from "./core/portal-appearance";
/** Renderer-owned attachment and state over the portable portal boundary. */
export function portalAppearance(open: () => boolean) {
  let source = $state<HTMLElement>(), attributes = $state<Record<string, string>>({}), container = $state<HTMLElement>();
  const anchor = { [createAttachmentKey()]: (element: HTMLElement) => { source = element; return () => { source = undefined; }; } };
  $effect(() => { if (!open() || !source) return; return observePortalAppearance(source, next => { attributes = next.attributes; container = next.container; }); });
  return { anchor, get attributes() { return attributes; }, get container() {
    // Mount directly in the native modal; keep that host while the popup closes.
    return open() && source ? modalPortalContainer(source) : container;
  } };
}
