import { tv, type VariantProps } from "tailwind-variants";
import { TONES, type Tone } from "./tone";

const VARIANT = {
  outline: {},
  plain: {
    root: "border-transparent",
    header: "border-transparent",
    footer: "border-transparent",
  },
} as const;

const RAISED = {
  false: {},
  true: { root: "shadow-elevation-raised" },
} as const;

// What a Card means (ADR 0026): the shared tone vocabulary, each tone on its
// Feedback Role's tinted surface (ADR 0023) — `info` is feedback-info here as
// on every other component, and a grey Card is `neutral`.
const TONE = {
  neutral: {
    root: "bg-elevation-raised-surface text-foreground",
    header: "text-foreground",
    title: "text-foreground",
    description: "text-ink-muted",
    body: "text-foreground",
    footer: "text-foreground",
  },
  success: {
    root: "bg-[var(--reddb-color-feedback-success-surface)] text-[var(--reddb-color-feedback-success-foreground)]",
    header: "text-[var(--reddb-color-feedback-success-foreground)]",
    title: "text-[var(--reddb-color-feedback-success-foreground)]",
    description: "text-[var(--reddb-color-feedback-success-foreground)]",
    body: "text-[var(--reddb-color-feedback-success-foreground)]",
    footer: "text-[var(--reddb-color-feedback-success-foreground)]",
  },
  warning: {
    root: "bg-[var(--reddb-color-feedback-warning-surface)] text-[var(--reddb-color-feedback-warning-foreground)]",
    header: "text-[var(--reddb-color-feedback-warning-foreground)]",
    title: "text-[var(--reddb-color-feedback-warning-foreground)]",
    description: "text-[var(--reddb-color-feedback-warning-foreground)]",
    body: "text-[var(--reddb-color-feedback-warning-foreground)]",
    footer: "text-[var(--reddb-color-feedback-warning-foreground)]",
  },
  danger: {
    root: "bg-[var(--reddb-color-feedback-danger-surface)] text-[var(--reddb-color-feedback-danger-foreground)]",
    header: "text-[var(--reddb-color-feedback-danger-foreground)]",
    title: "text-[var(--reddb-color-feedback-danger-foreground)]",
    description: "text-[var(--reddb-color-feedback-danger-foreground)]",
    body: "text-[var(--reddb-color-feedback-danger-foreground)]",
    footer: "text-[var(--reddb-color-feedback-danger-foreground)]",
  },
  info: {
    root: "bg-[var(--reddb-color-feedback-info-surface)] text-[var(--reddb-color-feedback-info-foreground)]",
    header: "text-[var(--reddb-color-feedback-info-foreground)]",
    title: "text-[var(--reddb-color-feedback-info-foreground)]",
    description: "text-[var(--reddb-color-feedback-info-foreground)]",
    body: "text-[var(--reddb-color-feedback-info-foreground)]",
    footer: "text-[var(--reddb-color-feedback-info-foreground)]",
  },
  /**
   * @deprecated The accent is an emphasis, not a tone (ADR 0026): kept one
   * release with its accent fill, then removed. Use a tone, or `neutral`.
   */
  brand: {
    root: "bg-[var(--reddb-color-primary)] text-[var(--reddb-color-on-primary)]",
    header: "text-[var(--reddb-color-on-primary)]",
    title: "text-[var(--reddb-color-on-primary)]",
    description: "text-[var(--reddb-color-on-primary)]",
    body: "text-[var(--reddb-color-on-primary)]",
    footer: "text-[var(--reddb-color-on-primary)]",
  },
} as const;

const PADDING = {
  none: { header: "p-0", body: "p-0", footer: "p-0" },
  sm: {
    header: "p-[var(--reddb-spatial-inset-sm)]",
    body: "p-[var(--reddb-spatial-inset-sm)]",
    footer: "p-[var(--reddb-spatial-inset-sm)]",
  },
  md: {
    header: "p-[var(--reddb-spatial-inset-md)]",
    body: "p-[var(--reddb-spatial-inset-md)]",
    footer: "p-[var(--reddb-spatial-inset-md)]",
  },
} as const;

const ACTIONS_ALIGN = {
  start: { footer: "justify-start" },
  center: { footer: "justify-center" },
  end: { footer: "justify-end" },
  between: { footer: "justify-between" },
} as const;

const ORIENTATION = {
  vertical: {
    root: "flex flex-col",
    layout: "flex min-w-0 flex-1 flex-col",
    media: "w-full",
    content: "flex min-w-0 flex-1 flex-col",
  },
  horizontal: {
    root: "",
    layout: "flex min-w-0 flex-1 flex-col @sm/card:flex-row",
    content: "flex min-w-0 flex-1 flex-col",
  },
} as const;

const MEDIA_FIT = {
  cover: { media: "[&>*]:object-cover" },
  contain: { media: "[&>*]:object-contain" },
} as const;

const MEDIA_POSITION = {
  center: { media: "[&>*]:object-center" },
  top: { media: "[&>*]:object-top" },
  bottom: { media: "[&>*]:object-bottom" },
  start: { media: "[&>*]:object-left" },
  end: { media: "[&>*]:object-right" },
} as const;

const MEDIA_RATIO = {
  "16/9": { media: "aspect-video [&>*]:h-full" },
  "4/3": { media: "aspect-[4/3] [&>*]:h-full" },
  "1/1": { media: "aspect-square [&>*]:h-full" },
  auto: { media: "aspect-auto [&>*]:h-auto" },
} as const;

const MEDIA_SPAN = {
  third: {},
  "two-fifths": {},
  half: {},
  background: {
    root: "relative isolate",
    media: "absolute inset-0 size-full",
    content: "relative z-10",
  },
} as const;

export const card = tv({
  slots: {
    root: "@container/card w-full overflow-hidden rounded-lg border",
    layout: "",
    media: "relative shrink-0 overflow-hidden [&>*]:block [&>*]:w-full",
    content: "",
    header: "flex flex-col gap-[var(--reddb-spatial-gap-sm)] border-b",
    titleRow: "flex items-center gap-[var(--reddb-spatial-gap-sm)]",
    icon: "shrink-0",
    title: "text-base font-medium leading-tight",
    description: "max-w-prose text-sm",
    // Keep native inline flow (text + emphasis is still one sentence) while
    // spacing direct body blocks. A flex column would split inline content.
    body: "flow-root flex-1 space-y-[var(--reddb-spatial-layout-gap-sm)]",
    footer: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-md)] border-t",
  },
  variants: {
    variant: VARIANT,
    raised: RAISED,
    tone: TONE,
    padding: PADDING,
    actionsAlign: ACTIONS_ALIGN,
    orientation: ORIENTATION,
    fit: MEDIA_FIT,
    position: MEDIA_POSITION,
    ratio: MEDIA_RATIO,
    span: MEDIA_SPAN,
  },
  compoundVariants: [
    {
      variant: "outline",
      tone: "neutral",
      class: { root: "border-muted", header: "border-muted", footer: "border-muted" },
    },
    {
      variant: "outline",
      tone: "brand",
      class: {
        root: "border-[var(--reddb-color-primary)]",
        header: "border-[var(--reddb-color-primary)]",
        footer: "border-[var(--reddb-color-primary)]",
      },
    },
    {
      variant: "outline",
      tone: "success",
      class: {
        root: "border-[var(--reddb-color-feedback-success-border)]",
        header: "border-[var(--reddb-color-feedback-success-border)]",
        footer: "border-[var(--reddb-color-feedback-success-border)]",
      },
    },
    {
      variant: "outline",
      tone: "warning",
      class: {
        root: "border-[var(--reddb-color-feedback-warning-border)]",
        header: "border-[var(--reddb-color-feedback-warning-border)]",
        footer: "border-[var(--reddb-color-feedback-warning-border)]",
      },
    },
    {
      variant: "outline",
      tone: "danger",
      class: {
        root: "border-[var(--reddb-color-feedback-danger-border)]",
        header: "border-[var(--reddb-color-feedback-danger-border)]",
        footer: "border-[var(--reddb-color-feedback-danger-border)]",
      },
    },
    {
      variant: "outline",
      tone: "info",
      class: {
        root: "border-[var(--reddb-color-feedback-info-border)]",
        header: "border-[var(--reddb-color-feedback-info-border)]",
        footer: "border-[var(--reddb-color-feedback-info-border)]",
      },
    },
    {
      raised: true,
      tone: "neutral",
      class: { root: "bg-elevation-raised-surface" },
    },
    {
      raised: true,
      variant: "outline",
      tone: "neutral",
      class: { root: "border-elevation-raised-border" },
    },
    {
      orientation: "horizontal",
      span: "third",
      class: { media: "@sm/card:w-1/3" },
    },
    {
      orientation: "horizontal",
      span: "two-fifths",
      class: { media: "@sm/card:w-2/5" },
    },
    {
      orientation: "horizontal",
      span: "half",
      class: { media: "@sm/card:w-1/2" },
    },
  ],
  defaultVariants: {
    variant: "outline",
    raised: false,
    tone: "neutral",
    padding: "md",
    actionsAlign: "start",
    orientation: "vertical",
    fit: "cover",
    position: "center",
    ratio: "16/9",
    span: "two-fifths",
  },
});

export type CardVariants = VariantProps<typeof card>;
export type CardVariant = NonNullable<CardVariants["variant"]>;
export type CardTone = Tone;
/** @deprecated `brand` is an emphasis, not a tone (ADR 0026). Removed next release. */
export type DeprecatedCardTone = (typeof DEPRECATED_CARD_TONES)[number];
export type CardPadding = NonNullable<CardVariants["padding"]>;
export type CardActionsAlign = NonNullable<CardVariants["actionsAlign"]>;
export type CardOrientation = NonNullable<CardVariants["orientation"]>;
export type CardMediaFit = NonNullable<CardVariants["fit"]>;
export type CardMediaPosition = NonNullable<CardVariants["position"]>;
export type CardMediaRatio = NonNullable<CardVariants["ratio"]>;
export type CardMediaSpan = NonNullable<CardVariants["span"]>;

export const CARD_VARIANTS = Object.keys(VARIANT) as readonly CardVariant[];
/** The tones a Card accepts: the shared vocabulary (ADR 0026). */
export const CARD_TONES = TONES satisfies readonly CardTone[];
/** Tone values kept for one release (ADR 0026): `brand` still draws its accent fill, and warns. */
export const DEPRECATED_CARD_TONES = ["brand"] as const;
export const CARD_PADDINGS = Object.keys(PADDING) as readonly CardPadding[];
export const CARD_ACTIONS_ALIGNMENTS = Object.keys(
  ACTIONS_ALIGN,
) as readonly CardActionsAlign[];
export const CARD_ORIENTATIONS = Object.keys(ORIENTATION) as readonly CardOrientation[];
export const CARD_MEDIA_FITS = Object.keys(MEDIA_FIT) as readonly CardMediaFit[];
export const CARD_MEDIA_POSITIONS = Object.keys(
  MEDIA_POSITION,
) as readonly CardMediaPosition[];
export const CARD_MEDIA_RATIOS = Object.keys(MEDIA_RATIO) as readonly CardMediaRatio[];
export const CARD_MEDIA_SPANS = Object.keys(MEDIA_SPAN) as readonly CardMediaSpan[];

/** Numeric image ratios shared by native Card adapters. */
export const CARD_RATIO_VALUES = { "16/9": 16 / 9, "4/3": 4 / 3, "1/1": 1 } as const;
