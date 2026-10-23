// Button's public appearance seam.
//
// Every class lives in these `tv()` contracts so a consumer can extend the
// canonical appearance without forking the element behavior. Colours resolve
// through Theme/Color Scheme roles and spatial values through Density roles;
// this module owns no appearance-axis selection or raw value.

import { tv, type VariantProps } from "tailwind-variants";
import { QUIET_HOVER, QUIET_OPEN, QUIET_PRESSED } from "./quiet-control.variants";
import type { Tone } from "./tone";
import { typeRoleMerge } from "./type-roles";

const VARIANT = {
  /** The affirmative action of a view — at most one per view. */
  primary: "bg-primary text-on-primary",
  /** Everything else that is still an action: outlined, not filled. */
  secondary: "border-control-edge bg-transparent text-foreground hover:border-foreground",
  /**
   * An action that should not compete for attention. It is the quiet-control
   * contract (quiet-control.variants.ts, wave 6A): transparent at rest, the
   * quiet fill on hover, the stronger one pressed or holding a popup open.
   */
  ghost: `bg-transparent text-ink-muted hover:text-foreground ${QUIET_HOVER} ${QUIET_PRESSED} ${QUIET_OPEN}`,
} as const;

// What the action means (ADR 0026): the shared tone vocabulary, resolved by
// the compound variants below onto each Feedback Role's materials.
const TONE = {
  neutral: "",
  info: "",
  success: "",
  warning: "",
  danger: "",
} as const satisfies Record<Tone, string>;

// A control owns a minimum rhythm, not a fixed text box: long labels and
// enlarged text may add lines without clipping or widening their container.
const SIZE = {
  sm: "min-h-[var(--reddb-spatial-control-height-sm)] px-[var(--reddb-spatial-inset-sm)] text-sm",
  md: "min-h-[var(--reddb-spatial-control-height-md)] px-[var(--reddb-spatial-inset-md)] text-sm",
  lg: "min-h-[var(--reddb-spatial-control-height-lg)] px-[var(--reddb-spatial-inset-lg)] text-base",
} as const;

const BLOCK = {
  /** Fills its column — a form's submit, a drawer's confirm, a mobile action. */
  true: "w-full",
  /** The default: as wide as what it says. */
  false: "",
} as const;

export const button = tv({
  base: [
    "inline-flex items-center justify-center gap-[var(--reddb-spatial-gap-md)]",
    "rounded-md border border-transparent",
    "h-auto min-w-0 max-w-full font-medium leading-tight whitespace-normal [overflow-wrap:anywhere]",
    // No colour transition (quiet-control.variants.ts): states and a Color
    // Scheme switch land at full contrast at once.
    // One ink focus ring for every variant and tone (ADR 0023): 2px, offset
    // 2px, so the pixels around it are the ground and never the Button's fill.
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    "disabled:pointer-events-none disabled:opacity-50",
    "aria-disabled:pointer-events-none aria-disabled:opacity-50",
  ].join(" "),
  variants: { variant: VARIANT, tone: TONE, size: SIZE, block: BLOCK },
  // Each tone keeps the variant's hierarchy: primary fills with the role's
  // fill material under its on-fill label, secondary outlines in the role's
  // border, ghost carries only the role's ink. A danger primary is therefore a
  // filled destructive action, deeper than the accent in both Color Schemes
  // and distinct from its secondary and ghost forms (ADR 0009, ADR 0023).
  //
  // A tone ghost keeps its role's tinted surface as its quiet fill, hovered and
  // pressed (wave 6A): the role's copy on the neutral ink fill drops to
  // 3.66:1 (danger) and 3.71:1 (info) on the dark overlay, where a danger
  // ghost sits in a Dialog, while the contract holds it on its own surface.
  //
  // A filled Button never hovers with an opacity (wave 6A): every filled
  // emphasis swaps to its opaque active material — `primary-active` for the
  // accent, `feedback-<tone>-fill-active` for a tone — which the contrast
  // contract pairs with the label, so the ground never shows through.
  compoundVariants: [
    {
      // Hovered, pressed or holding a popup open, the accent fills with the
      // opaque `primary-active` role, which the contrast contract pairs with
      // `on-primary` (wave 5A). The old opacity hover let the ground show
      // through: a SpeedDial trigger held open on Marketing dark fell to 4.44:1.
      tone: "neutral",
      variant: "primary",
      class: "hover:bg-primary-active active:bg-primary-active aria-expanded:bg-primary-active",
    },
    {
      tone: "danger",
      variant: "primary",
      class: "bg-feedback-danger-fill text-feedback-danger-on-fill hover:bg-feedback-danger-fill-active active:bg-feedback-danger-fill-active aria-expanded:bg-feedback-danger-fill-active",
    },
    {
      tone: "danger",
      variant: "secondary",
      class: "border-feedback-danger-border text-feedback-danger-foreground hover:border-feedback-danger-foreground hover:bg-feedback-danger-surface",
    },
    {
      tone: "danger",
      variant: "ghost",
      class: "text-feedback-danger-foreground hover:text-feedback-danger-foreground hover:bg-feedback-danger-surface active:bg-feedback-danger-surface",
    },
    {
      tone: "success",
      variant: "primary",
      class: "bg-feedback-success-fill text-feedback-success-on-fill hover:bg-feedback-success-fill-active active:bg-feedback-success-fill-active aria-expanded:bg-feedback-success-fill-active",
    },
    {
      tone: "success",
      variant: "secondary",
      class: "border-feedback-success-border text-feedback-success-foreground hover:border-feedback-success-foreground hover:bg-feedback-success-surface",
    },
    {
      tone: "success",
      variant: "ghost",
      class: "text-feedback-success-foreground hover:text-feedback-success-foreground hover:bg-feedback-success-surface active:bg-feedback-success-surface",
    },
    {
      tone: "warning",
      variant: "primary",
      class: "bg-feedback-warning-fill text-feedback-warning-on-fill hover:bg-feedback-warning-fill-active active:bg-feedback-warning-fill-active aria-expanded:bg-feedback-warning-fill-active",
    },
    {
      tone: "warning",
      variant: "secondary",
      class: "border-feedback-warning-border text-feedback-warning-foreground hover:border-feedback-warning-foreground hover:bg-feedback-warning-surface",
    },
    {
      tone: "warning",
      variant: "ghost",
      class: "text-feedback-warning-foreground hover:text-feedback-warning-foreground hover:bg-feedback-warning-surface active:bg-feedback-warning-surface",
    },
    {
      tone: "info",
      variant: "primary",
      class: "bg-feedback-info-fill text-feedback-info-on-fill hover:bg-feedback-info-fill-active active:bg-feedback-info-fill-active aria-expanded:bg-feedback-info-fill-active",
    },
    {
      tone: "info",
      variant: "secondary",
      class: "border-feedback-info-border text-feedback-info-foreground hover:border-feedback-info-foreground hover:bg-feedback-info-surface",
    },
    {
      tone: "info",
      variant: "ghost",
      class: "text-feedback-info-foreground hover:text-feedback-info-foreground hover:bg-feedback-info-surface active:bg-feedback-info-surface",
    },
  ],
  defaultVariants: { variant: "secondary", tone: "neutral", size: "md", block: false },
}, typeRoleMerge);

/**
 * The loading indicator, inheriting the Button variant's current colour. It
 * takes the Icon size role of the Button's size, so it matches a leading Icon
 * at every Density stop and the label does not shift when loading starts.
 */
export const buttonSpinner = tv({
  slots: {
    root: "shrink-0 motion-safe:animate-spin motion-reduce:animate-pulse",
    track: "fill-none stroke-current opacity-25",
    head: "fill-none stroke-current",
  },
  variants: {
    size: {
      sm: { root: "size-[var(--reddb-spatial-icon-size-sm)]" },
      md: { root: "size-[var(--reddb-spatial-icon-size-md)]" },
      lg: { root: "size-[var(--reddb-spatial-icon-size-lg)]" },
    },
  },
  defaultVariants: { size: "md" },
});

export type ButtonVariants = VariantProps<typeof button>;
export type ButtonVariant = NonNullable<ButtonVariants["variant"]>;
export type ButtonTone = NonNullable<ButtonVariants["tone"]>;
/** @deprecated Button's meaning is `tone` (ADR 0026); use `ButtonTone`. Removed next release. */
export type ButtonIntent = ButtonTone;
export type ButtonSize = NonNullable<ButtonVariants["size"]>;

// Enumerated from the maps so documentation and local extensions can inspect
// the same closed vocabulary the component accepts without restating it.
export const BUTTON_VARIANTS = Object.keys(VARIANT) as readonly ButtonVariant[];
export const BUTTON_TONES = Object.keys(TONE) as readonly ButtonTone[];
/** @deprecated Button's meaning is `tone` (ADR 0026); use `BUTTON_TONES`. Removed next release. */
export const BUTTON_INTENTS = BUTTON_TONES;
export const BUTTON_SIZES = Object.keys(SIZE) as readonly ButtonSize[];
