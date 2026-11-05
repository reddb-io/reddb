/** Copy exact source; unavailable or denied clipboard access is a recoverable result. */
export async function copyCodeSource(code: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(code);
    return true;
  } catch {
    return false;
  }
}

/** Measure again on content/layout updates; dispose observers with the native adapter. */
export function observeCodeOverflow(element: HTMLPreElement, changed: (overflowing: boolean) => void): () => void {
  const measure = () => changed(element.scrollWidth > element.clientWidth || element.scrollHeight > element.clientHeight);
  measure();
  if (typeof ResizeObserver === "undefined") {
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }
  const observer = new ResizeObserver(measure);
  observer.observe(element);
  if (element.firstElementChild) observer.observe(element.firstElementChild);
  return () => observer.disconnect();
}
