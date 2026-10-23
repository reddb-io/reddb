import { tv, type VariantProps } from "tailwind-variants";

/** The complete daisyUI Mask shape and half-mask vocabulary. */
export const CONTENT_MASK_SHAPES = [
  "squircle",
  "heart",
  "hexagon",
  "hexagon-2",
  "decagon",
  "pentagon",
  "diamond",
  "square",
  "circle",
  "star",
  "star-2",
  "triangle",
  "triangle-2",
  "triangle-3",
  "triangle-4",
  "half-1",
  "half-2",
] as const;

export type ContentMaskShape = (typeof CONTENT_MASK_SHAPES)[number];

/** Structural clipping geometry. It names no appearance token or axis. */
export const CONTENT_MASK_CLIP_PATHS: Readonly<Record<ContentMaskShape, string>> = {
  squircle:
    "polygon(50% 0%, 80% 5%, 95% 20%, 100% 50%, 95% 80%, 80% 95%, 50% 100%, 20% 95%, 5% 80%, 0% 50%, 5% 20%, 20% 5%)",
  heart:
    "polygon(50% 100%, 5% 55%, 0% 35%, 5% 15%, 20% 5%, 35% 5%, 50% 20%, 65% 5%, 80% 5%, 95% 15%, 100% 35%, 95% 55%)",
  hexagon: "polygon(25% 0%, 75% 0%, 100% 50%, 75% 100%, 25% 100%, 0% 50%)",
  "hexagon-2": "polygon(50% 0%, 100% 25%, 100% 75%, 50% 100%, 0% 75%, 0% 25%)",
  decagon:
    "polygon(50% 0%, 79% 10%, 98% 35%, 98% 65%, 79% 90%, 50% 100%, 21% 90%, 2% 65%, 2% 35%, 21% 10%)",
  pentagon: "polygon(50% 0%, 100% 38%, 82% 100%, 18% 100%, 0% 38%)",
  diamond: "polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%)",
  square: "inset(0%)",
  circle: "circle(50%)",
  star:
    "polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 94%, 50% 72%, 21% 94%, 32% 57%, 2% 35%, 39% 35%)",
  "star-2":
    "polygon(50% 0%, 64% 29%, 98% 35%, 73% 59%, 79% 94%, 50% 78%, 21% 94%, 27% 59%, 2% 35%, 36% 29%)",
  triangle: "polygon(50% 0%, 100% 100%, 0% 100%)",
  "triangle-2": "polygon(0% 0%, 100% 0%, 50% 100%)",
  "triangle-3": "polygon(0% 50%, 100% 0%, 100% 100%)",
  "triangle-4": "polygon(0% 0%, 100% 50%, 0% 100%)",
  "half-1": "inset(0% 50% 0% 0%)",
  "half-2": "inset(0% 0% 0% 50%)",
};

/** Extension seam for the neutral clipping boundary. */
export const contentMask = tv({
  base: "inline-grid",
});

export type ContentMaskVariants = VariantProps<typeof contentMask>;
