export const REVEAL_DIRECTIONS = ["up", "down", "left", "right", "none"] as const;
export type RevealDirection = typeof REVEAL_DIRECTIONS[number];
export interface RevealState { path: "css" | "js"; entered: boolean; reducedMotion: boolean }
/** Useful, visible SSR; adapters adopt the same initial state before native effects run. */
export function initialRevealState(): RevealState { return { path: "js", entered: true, reducedMotion: false }; }
export function revealDelay(delay: number): number {
  if (!Number.isFinite(delay) || delay < 0) throw new Error("Reveal: delay must be finite and nonnegative.");
  return delay;
}
const offsets: Record<RevealDirection, readonly [string, string]> = {
  up: ["0px", "var(--reddb-spatial-inset-lg)"],
  down: ["0px", "calc(-1 * var(--reddb-spatial-inset-lg))"],
  left: ["calc(-1 * var(--reddb-spatial-inset-lg))", "0px"],
  right: ["var(--reddb-spatial-inset-lg)", "0px"],
  none: ["0px", "0px"],
};

/** Native browser behavior with no rendering runtime; disposed by each adapter. */
export function mountReveal(element: HTMLElement, direction: RevealDirection, delay: number, changed: (state: RevealState) => void): () => void {
  // Millisecond delays belong to a timed entry transition, not a progress timeline.
  const path = delay === 0 && typeof CSS !== "undefined" && CSS.supports("animation-timeline: view()") ? "css" : "js";
  const preference = typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : undefined;
  let animation: Animation | undefined, observer: IntersectionObserver | undefined;
  let disposed = false, settled = false;
  function stop() { animation?.cancel(); animation = undefined; observer?.disconnect(); observer = undefined; }
  function sync() {
    stop();
    const reducedMotion = preference?.matches ?? false;
    if (reducedMotion || settled) { settled = true; changed({ path, entered: true, reducedMotion }); return; }
    if (path === "css") {
      const ViewTimeline = (globalThis as typeof globalThis & { ViewTimeline?: new (options: { subject: Element }) => AnimationTimeline }).ViewTimeline;
      if (typeof element.animate !== "function" || !ViewTimeline) { changed({ path, entered: true, reducedMotion }); return; }
      try {
        const [x, y] = offsets[direction];
        animation = element.animate([{ opacity: 0, transform: `translate3d(${x}, ${y}, 0)` }, { opacity: 1, transform: "translate3d(0, 0, 0)" }], {
          duration: 1, easing: "cubic-bezier(0.22, 1, 0.36, 1)", fill: "both", timeline: new ViewTimeline({ subject: element }), rangeStart: "entry 0%", rangeEnd: "entry 35%",
        } as KeyframeAnimationOptions);
        changed({ path, entered: false, reducedMotion });
      } catch { changed({ path, entered: true, reducedMotion }); }
      return;
    }
    if (typeof IntersectionObserver === "undefined") { changed({ path, entered: true, reducedMotion }); return; }
    changed({ path, entered: false, reducedMotion });
    observer = new IntersectionObserver(entries => {
      if (disposed || !entries.some(entry => entry.isIntersecting)) return;
      settled = true;
      changed({ path, entered: true, reducedMotion: preference?.matches ?? false });
      observer?.disconnect(); observer = undefined;
    });
    observer.observe(element);
  }
  sync();
  if (preference?.addEventListener) preference.addEventListener("change", sync);
  else preference?.addListener(sync);
  return () => {
    disposed = true; stop();
    if (preference?.removeEventListener) preference.removeEventListener("change", sync);
    else preference?.removeListener(sync);
  };
}
