// The slide layouts: TitleSlide, SectionSlide, TwoColumnSlide, FigureSlide,
// QuoteSlide and BulletsSlide (ADR 0027).
//
// Each is a SlideFrame with its parts placed inside the safe title area. Type
// comes only from the Theme's roles (ADR 0025): a slide title is display or
// title, supporting copy heading or body, attribution and captions caption.
// Rhythm comes from the layout tier (ADR 0024), which the SlideFrame pins to
// the frame, so a layout keeps its proportions at every Density and scales
// with the frame like everything else on the canvas.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

export const slideLayout = tv(
  {
    slots: {
      // The layout fills the safe area; its parts never push past it.
      body: "flex min-h-0 min-w-0 flex-1 flex-col gap-[var(--reddb-spatial-layout-gap-lg)]",
      // An eyebrow, title and lede read as one group.
      header: "flex min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-sm)]",
      // A title or section slide centres its header group in the safe area.
      centred: "my-auto",
      // Supporting copy under a title: the heading role in secondary ink.
      lede: "text-heading text-ink-muted text-pretty",
      // The presenter line or attribution at the foot of the slide.
      foot: "mt-auto",
      // Two equal columns that share the remaining height.
      columns: "grid min-h-0 flex-1 grid-cols-2 gap-[var(--reddb-spatial-layout-gap-lg)]",
      column: "flex min-h-0 min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-sm)]",
      // A figure takes the remaining height; its caption sits under it.
      figure: "flex min-h-0 flex-1 flex-col gap-[var(--reddb-spatial-layout-gap-sm)]",
      figureBody: "relative min-h-0 min-w-0 flex-1",
      // A quotation, in the title role, set as a pull quote.
      quote: "my-auto flex min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-md)]",
      quoteText: "text-title text-foreground text-balance",
      // Points at the heading role, markers in secondary ink.
      list: "flex min-w-0 list-disc flex-col gap-[var(--reddb-spatial-layout-gap-sm)] ps-[var(--reddb-spatial-layout-gap-md)] text-heading text-foreground marker:text-ink-muted",
      item: "ps-[var(--reddb-spatial-gap-sm)] text-pretty",
    },
  },
  typeRoleMerge,
);

export type SlideLayoutVariants = VariantProps<typeof slideLayout>;
