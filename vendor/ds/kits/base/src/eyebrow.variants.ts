// Eyebrow: the uppercase micro-label above a heading (ADR 0025, DESIGN.md).
//
// Size, tracking and weight are the Theme's eyebrow role — one tracking, the
// Brand's `wide`, instead of the three the Kits used to spell by hand — and
// the colour is the secondary reading role, `ink-muted`.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

export const eyebrow = tv({ base: "text-eyebrow uppercase text-ink-muted" }, typeRoleMerge);

export type EyebrowVariants = VariantProps<typeof eyebrow>;
/** The elements an Eyebrow may render: it labels a heading, it is never one. */
export const EYEBROW_ELEMENTS = ["p", "span", "div"] as const;
export type EyebrowElement = (typeof EYEBROW_ELEMENTS)[number];
