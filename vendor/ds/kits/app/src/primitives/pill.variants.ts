// Pill's styling. See button.variants.ts for the split, the colour rule and the
// spatial rule — under which only the medium size's inset lands on a role the
// axis ships. The small size is a notch below the narrowest one, and a value
// the axis has no role for stays a step rather than being pushed onto the
// nearest, which would move what the neutral stop renders.
//
// A Pill is round-ended (`rounded-full`) and stands on its own: a filter that
// is on, a tag, a selected facet. That is why it — unlike Badge — has a size
// axis and a dismissible affordance.
//
// What a Pill means is `tone`, the one semantic vocabulary (ADR 0026), and how
// strongly it is drawn is `variant`, its emphasis axis — the same two axes, and
// the same pairs, as the Base Badge, so a `tone="warning"` Pill and a
// `tone="warning"` Badge are one Feedback Role. No tone fills with the Brand
// red: red on a status is danger (ADR 0023).

import { TONES, type Tone } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

/** How strongly a Pill's tone is drawn — its emphasis axis. */
export const PILL_VARIANTS = ["tinted", "filled", "outline"] as const;
export type PillVariant = (typeof PILL_VARIANTS)[number];

/** The tones a Pill accepts: the shared vocabulary (ADR 0026). */
export const PILL_TONES = TONES;
export type PillTone = Tone;

/**
 * Variant values kept for one release (ADR 0026). `neutral` is the tinted
 * neutral pill it always drew; `primary` keeps its accent fill and warns — a
 * meaning is a tone (`tone="info" variant="filled"`), never the accent.
 */
export const DEPRECATED_PILL_VARIANTS = {
  neutral: "tinted",
  primary: "filled",
} as const satisfies Record<string, PillVariant>;
/** @deprecated `neutral` is `variant="tinted"`; `primary` is a filled tone. Removed next release. */
export type DeprecatedPillVariant = keyof typeof DEPRECATED_PILL_VARIANTS;

const APPEARANCE = {
  tinted: {
    neutral: "border-transparent bg-muted text-foreground",
    info: "border-transparent bg-feedback-info-surface text-feedback-info-foreground",
    success: "border-transparent bg-feedback-success-surface text-feedback-success-foreground",
    warning: "border-transparent bg-feedback-warning-surface text-feedback-warning-foreground",
    danger: "border-transparent bg-feedback-danger-surface text-feedback-danger-foreground",
  },
  filled: {
    neutral: "border-transparent bg-foreground text-background",
    info: "border-transparent bg-feedback-info-foreground text-on-feedback",
    success: "border-transparent bg-feedback-success-foreground text-on-feedback",
    warning: "border-transparent bg-feedback-warning-foreground text-on-feedback",
    danger: "border-transparent bg-feedback-danger-foreground text-on-feedback",
  },
  outline: {
    neutral: "border-muted bg-transparent text-foreground",
    info: "border-feedback-info-border bg-transparent text-feedback-info-foreground",
    success: "border-feedback-success-border bg-transparent text-feedback-success-foreground",
    warning: "border-feedback-warning-border bg-transparent text-feedback-warning-foreground",
    danger: "border-feedback-danger-border bg-transparent text-feedback-danger-foreground",
  },
} as const satisfies Record<PillVariant, Record<Tone, string>>;

/** The accent fill the deprecated `variant="primary"` still draws for one release. */
const DEPRECATED_PRIMARY = "border-transparent bg-primary text-on-primary";

const TONE = { neutral: "", info: "", success: "", warning: "", danger: "" } as const satisfies Record<
  Tone,
  string
>;
const VARIANT = { tinted: "", filled: "", outline: "", neutral: "", primary: "" } as const;

const SIZE = {
  sm: "px-2.5 py-0.5 text-xs",
  md: "px-[var(--reddb-spatial-inset-sm)] py-1 text-sm",
} as const;

export const pill = tv({
  base: "inline-flex items-center gap-1.5 rounded-full border leading-tight whitespace-nowrap",
  variants: { tone: TONE, variant: VARIANT, size: SIZE },
  compoundVariants: [
    ...(Object.entries(APPEARANCE) as [PillVariant, Record<Tone, string>][]).flatMap(([variant, tones]) =>
      (Object.entries(tones) as [Tone, string][]).map(([tone, className]) => ({
        variant,
        tone,
        class: className,
      })),
    ),
    // The deprecated neutral variant is the tinted pill, tone for tone.
    ...(Object.entries(APPEARANCE.tinted) as [Tone, string][]).map(([tone, className]) => ({
      variant: "neutral" as const,
      tone,
      class: className,
    })),
    { variant: "primary" as const, class: DEPRECATED_PRIMARY },
  ],
  defaultVariants: { tone: "neutral", variant: "tinted", size: "md" },
});

/**
 * The dismiss affordance, styled to inherit the Pill's own colour. The glyph
 * stays small, but its hit area is a control-height-sm square centred on it,
 * so the target never drops below 24px (WCAG 2.2 SC 2.5.8). Its rest, hover,
 * pressed and focus states are the quiet-control contract the component wraps
 * it in (wave 6A).
 */
export const pillDismiss = tv({
  base: [
    "relative -me-1 inline-flex items-center justify-center rounded-full border border-transparent bg-transparent p-0.5 leading-tight",
    "before:absolute before:left-1/2 before:top-1/2 before:size-[var(--reddb-spatial-control-height-sm)] before:-translate-x-1/2 before:-translate-y-1/2 before:content-['']",
  ].join(" "),
});

export type PillVariants = VariantProps<typeof pill>;
export type PillSize = keyof typeof SIZE;

export const PILL_SIZES = Object.keys(SIZE) as readonly PillSize[];
