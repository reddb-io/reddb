// SlideFrame: a fixed 16:9 frame for presentations (ADR 0027), a fixed-aspect
// size container (ADR 0021) whose content scales with the frame's own width.
//
// HOW CONTENT SCALES — A ZOOMED CANVAS, NOT A TRANSFORM AND NOT PER-ROLE cqi.
//
// Inside the frame sits a stage: a fixed 60rem × 33.75rem canvas (the 16:9
// page every slide tool uses, 13.33in at 72pt/in) that is laid out exactly once
// and scaled to the frame with CSS `zoom`. The zoom factor is the ratio of the
// frame's width to the canvas width, computed in CSS alone:
// `tan(atan2(100cqi, 60rem))` turns two lengths into a unitless number (atan2
// takes the two lengths, tan gives back their ratio), and `100cqi` is the
// frame's inline size because the frame root is the size container.
//
//   - Why not per-role cqi type. Scaling only the type roles leaves every
//     Density inset, gap and control height at page size: a 360px frame is
//     202px tall, and fixed rhythm overflows it long before the type does. The
//     Marketing roles are already fluid against their container, so a cqi
//     multiplier would scale them twice. A canvas scales type, rhythm, borders
//     and nested components together, so a slide that fits at one width fits
//     at every width.
//   - Why not `transform: scale()`. A transform leaves the layout box at
//     canvas size, so the frame needs a measured wrapper height, hit testing
//     and focus rings answer to the unscaled box, and text is rasterised once
//     and resampled. `zoom` changes the used size of everything inside, so the
//     browser lays text out at the size it paints it — crisp at every width —
//     and the frame's own box stays the 16:9 the page asked for.
//
// The Theme's type roles are untouched: a slide reads in the roles of whatever
// Theme surrounds it, measured on the canvas. Marketing's fluid display and
// title interpolate against the stage (a 60rem container), so on a slide they
// sit near their large stop at every frame width instead of shrinking twice.
// Without trigonometric CSS functions the zoom is ignored, and the root's
// aspect ratio and overflow clip keep the frame 16:9 rather than letting the
// canvas push the page wide.
//
// RHYTHM IS PINNED TO THE FRAME, NOT TO DENSITY. A slide is a fixed
// composition like a printed page: the deck an author fitted at one Density
// must not overflow at `spacious` on a reader's machine. The stage re-declares
// the layout and section tiers (ADR 0024) at the neutral stop's Brand steps,
// so the safe area and every layout inside a slide keep the same proportions
// at every Density. The component tier is not pinned: a Badge or a Button
// placed on a slide still follows the surrounding Density, scaled with the
// canvas. The frame never selects an axis — it sets no `data-density`, and a
// caller's own Density island inside a slide still re-resolves.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

/** The canvas a slide is composed on, in rem: 60rem × 33.75rem (16:9). */
export const SLIDE_CANVAS_WIDTH_REM = 60;

/**
 * The layout and section tiers on a slide, pinned to the `comfortable` stop's
 * Brand steps (ADR 0024) so slide rhythm is the frame's, not the Density's.
 */
const PINNED_RHYTHM = [
  "[--reddb-spatial-layout-gap-sm:var(--reddb-spacing-4)]",
  "[--reddb-spatial-layout-gap-md:var(--reddb-spacing-6)]",
  "[--reddb-spatial-layout-gap-lg:var(--reddb-spacing-8)]",
  "[--reddb-spatial-section-gap:var(--reddb-spacing-16)]",
  "[--reddb-spatial-section-inset:var(--reddb-spacing-12)]",
].join(" ");

/** The ground a slide is set on: the page background or the raised elevation. */
const SURFACE = {
  background: { root: "bg-background after:border-muted" },
  raised: { root: "bg-elevation-raised-surface after:border-elevation-raised-border shadow-elevation-raised" },
} as const;

export const slideFrame = tv(
  {
    slots: {
      // The size container the stage measures. `w-full` because a size
      // container takes its width from its parent (ADR 0021); the aspect ratio
      // and clip hold the frame even if the zoom is unsupported. The hairline
      // edge is drawn on an overlay, not as a border, so the frame's content
      // box is exactly 16:9 and the canvas fills it without a sliver.
      root: "@container/slide-frame relative block w-full min-w-0 aspect-video overflow-hidden rounded-lg text-foreground break-inside-avoid after:pointer-events-none after:absolute after:inset-0 after:rounded-lg after:border after:content-['']",
      // The canvas, zoomed to the frame. It is itself a container, so anything
      // composed on a slide lays out against the slide (ADR 0021).
      stage: `@container/slide relative box-border flex w-[60rem] aspect-video flex-col [zoom:tan(atan2(100cqi,60rem))] p-[var(--reddb-spatial-section-inset)] text-body ${PINNED_RHYTHM}`,
      // The safe title area: everything a layout places stays inside it.
      safeArea: "relative flex min-h-0 min-w-0 flex-1 flex-col",
      // The slide number sits in the bottom margin, outside the safe area.
      number: "absolute inset-x-0 bottom-0 flex h-[var(--reddb-spatial-section-inset)] items-center justify-end px-[var(--reddb-spatial-section-inset)] text-caption tabular-nums text-ink-muted",
    },
    variants: { surface: SURFACE },
    defaultVariants: { surface: "background" },
  },
  typeRoleMerge,
);

export type SlideFrameVariants = VariantProps<typeof slideFrame>;
export type SlideSurface = NonNullable<SlideFrameVariants["surface"]>;
export const SLIDE_SURFACES = Object.keys(SURFACE) as readonly SlideSurface[];
