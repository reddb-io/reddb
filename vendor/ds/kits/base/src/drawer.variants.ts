import { tv, type VariantProps } from "tailwind-variants";
import { dialog } from "./dialog.variants";

export const DRAWER_SIDES = ["top", "right", "bottom", "left"] as const;
export type DrawerSide = (typeof DRAWER_SIDES)[number];

// A modal <dialog> arrives from the user agent with `inset: 0` on every edge,
// so pinning one edge is not enough: the sized panel is over-constrained and
// keeps its top and inline-start edges, opening a `right` Drawer on the left
// and a `bottom` one at the top. Each side pins its own edge and releases the
// opposite one, which also holds the panel in place while it leaves the top
// layer. Closed, and in its first frame, the panel sits just past that edge,
// so it enters from it and leaves toward it; under reduced motion it never moves.
const SIDE: Record<DrawerSide, string> = {
  top: [
    "inset-x-0 top-0 bottom-auto w-full max-w-none",
    "motion-safe:not-open:-translate-y-full motion-safe:starting:open:-translate-y-full",
  ].join(" "),
  right: [
    "inset-y-0 right-0 left-auto h-full w-full max-w-md",
    "motion-safe:not-open:translate-x-full motion-safe:starting:open:translate-x-full",
  ].join(" "),
  bottom: [
    "inset-x-0 top-auto bottom-0 w-full max-w-none",
    "motion-safe:not-open:translate-y-full motion-safe:starting:open:translate-y-full",
  ].join(" "),
  left: [
    "inset-y-0 left-0 right-auto h-full w-full max-w-md",
    "motion-safe:not-open:-translate-x-full motion-safe:starting:open:-translate-x-full",
  ].join(" "),
};

/** Dialog appearance specialized into an edge-anchored surface. */
export const drawer = tv({
  base: [
    dialog(),
    // An over-tall sheet scrolls inside the viewport instead of running past it.
    "fixed m-0 max-h-full overflow-y-auto rounded-none",
    // `display` and `overlay` change discretely, so the panel stays painted in
    // the top layer until its slide out has finished.
    "motion-safe:transition-[translate,display,overlay] motion-safe:transition-discrete",
    "motion-safe:duration-[var(--reddb-duration-normal)] motion-safe:ease-exit motion-safe:open:ease-entrance",
  ].join(" "),
  variants: { side: SIDE },
  defaultVariants: { side: "right" },
});

export type DrawerVariants = VariantProps<typeof drawer>;
