// Badge's public appearance seam: what a status means (`tone`, ADR 0026) and
// how strongly it is drawn (`variant`, its emphasis axis) are two axes.
//
// Every tone resolves to its Feedback Role exactly as it does on Alert, Card
// and Button, through pairs the Foundation's contrast contract measures:
//   - tinted: the role's copy on its tinted message surface (ADR 0023);
//   - filled: `on-feedback` ink on the role's text-grade stop — a tone chip;
//   - outline: the role's copy inside its border, on whatever ground it sits.
// Neutral is ink on the muted surface, ink as a fill, or ink in a muted edge.
// No tone fills with the Brand red: red on a status is danger (DESIGN.md).
import { tv, type VariantProps } from "tailwind-variants";
import type { Tone } from "./tone";

/** How strongly a Badge's tone is drawn — its emphasis axis. */
export const BADGE_VARIANTS = ["tinted", "filled", "outline"] as const;
export type BadgeVariant = (typeof BADGE_VARIANTS)[number];

/**
 * Variant values kept for one release (ADR 0026). `neutral` is the tinted
 * neutral badge it always drew; `primary` keeps its accent fill and warns —
 * a status is a tone (`tone="danger" variant="filled"`), never the accent.
 */
export const DEPRECATED_BADGE_VARIANTS = {
  neutral: "tinted",
  primary: "filled",
} as const satisfies Record<string, BadgeVariant>;
/** @deprecated `neutral` is `variant="tinted"`; `primary` is a filled tone. Removed next release. */
export type DeprecatedBadgeVariant = keyof typeof DEPRECATED_BADGE_VARIANTS;

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
} as const satisfies Record<BadgeVariant, Record<Tone, string>>;

/** The accent fill the deprecated `variant="primary"` still draws for one release. */
const DEPRECATED_PRIMARY = "border-transparent bg-primary text-on-primary";

const TONE = { neutral: "", info: "", success: "", warning: "", danger: "" } as const satisfies Record<
  Tone,
  string
>;
const VARIANT = { tinted: "", filled: "", outline: "", neutral: "", primary: "" } as const;

export const badge = tv({
  base: [
    "inline-flex items-center gap-[var(--reddb-spatial-gap-sm)] rounded-md border px-2 py-0.5",
    "text-xs font-medium leading-tight whitespace-nowrap",
  ].join(" "),
  variants: { tone: TONE, variant: VARIANT },
  compoundVariants: [
    ...(Object.entries(APPEARANCE) as [BadgeVariant, Record<Tone, string>][]).flatMap(([variant, tones]) =>
      (Object.entries(tones) as [Tone, string][]).map(([tone, className]) => ({
        variant,
        tone,
        class: className,
      })),
    ),
    // The deprecated neutral variant is the tinted badge, tone for tone.
    ...(Object.entries(APPEARANCE.tinted) as [Tone, string][]).map(([tone, className]) => ({
      variant: "neutral" as const,
      tone,
      class: className,
    })),
    { variant: "primary" as const, class: DEPRECATED_PRIMARY },
  ],
  defaultVariants: { tone: "neutral", variant: "tinted" },
});

export type BadgeVariants = VariantProps<typeof badge>;
export type BadgeTone = Tone;
