import type { SectionHeadingLevel, SectionHeadingSize } from "@reddb-io/design-system/base";

/** Portable CardHeading properties shared by native adapters. */
export interface CardHeadingContract {
/** Heading text, rendered as a heading element at `level`. */
    title: string;
/** Supporting line under the title. Nothing is rendered when omitted. */
    description?: string;
/**
     * Outline depth of the heading, `1` to `6`, independent of its visual `size`. Defaults to `2`.
     */
    level?: SectionHeadingLevel;
/** Visual size of the title: `sm`, `md`, `lg` or `display`. Defaults to `sm`. */
    size?: SectionHeadingSize;
}
