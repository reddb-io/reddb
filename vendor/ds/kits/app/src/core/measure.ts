/** Measure real chrome geometry and keep it synchronized without owning a rendering framework. */
export function observeElementSize(element: HTMLElement | null | undefined, changed: (rect: DOMRectReadOnly) => void): () => void {
  if (!element) return () => {};
  const measure = () => changed(element.getBoundingClientRect()); measure();
  if (typeof ResizeObserver === "undefined") return () => {};
  const observer = new ResizeObserver(measure); observer.observe(element); return () => observer.disconnect();
}
