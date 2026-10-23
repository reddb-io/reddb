/** Resolve before the first popup render so native modal portals never mount on body first. */
export function modalPortalContainer(source: HTMLElement): HTMLElement | undefined {
  return source.closest<HTMLDialogElement>("dialog[open]") ?? undefined;
}
/** The portal retains its source's independent appearance axes and native modal boundary. */
export function observePortalAppearance(source: HTMLElement, changed: (state: { attributes: Record<string, string>; container: HTMLElement | undefined }) => void) {
  const keys = ["data-theme", "data-color-scheme", "data-density"];
  const read = () => changed({
    attributes: Object.fromEntries(keys.flatMap(key => { const value = source.closest(`[${key}]`)?.getAttribute(key); return value == null ? [] : [[key, value]]; })),
    container: modalPortalContainer(source),
  });
  read(); const observer = new MutationObserver(read);
  for (let node: HTMLElement | null = source; node; node = node.parentElement) observer.observe(node, { attributes: true, attributeFilter: [...keys, "open"] });
  return () => observer.disconnect();
}
