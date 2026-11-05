import type { CardPadding, CardVariant, SectionHeadingLevel } from "@reddb-io/design-system/base";

/** Portable ActionPanel properties shared by native adapters. */
export interface ActionPanelContract {
/**
     * Panel name: rendered as the heading, and used as the region's accessible name
     * unless `aria-label` is given.
     */
    title: string;
/** Supporting line under the heading. Nothing is rendered when omitted. */
    description?: string;
/**
     * Outline depth of the heading, `1` to `6`; it is always drawn at the compact size. Defaults to
     * `2`.
     */
    level?: SectionHeadingLevel;
/** Edge treatment of the underlying Card: `outline` or `plain`. Defaults to `outline`. */
    variant?: CardVariant;
/**
     * Density-owned inset of the header, content and footer: `none`, `sm` or `md`. Defaults to
     * `md`.
     */
    padding?: CardPadding;
}
