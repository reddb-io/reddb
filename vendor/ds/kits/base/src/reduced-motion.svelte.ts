/** The platform query shared by every Motion Primitive. */
export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

export interface ReducedMotionPreference {
  readonly current: boolean;
}

/**
 * A component-scoped reactive view of the reader's motion preference.
 *
 * The browser media query remains the source of truth. The rune mirrors it so
 * a mounted component follows preference changes without owning listeners.
 */
export function useReducedMotion(): ReducedMotionPreference {
  let current = $state(false);

  $effect(() => {
    if (typeof globalThis.matchMedia !== "function") return;

    const query = globalThis.matchMedia(REDUCED_MOTION_QUERY);
    const sync = () => {
      current = query.matches;
    };
    sync();

    if (typeof query.addEventListener === "function") {
      query.addEventListener("change", sync);
      return () => query.removeEventListener("change", sync);
    }

    query.addListener(sync);
    return () => query.removeListener(sync);
  });

  return {
    get current() {
      return current;
    },
  };
}
