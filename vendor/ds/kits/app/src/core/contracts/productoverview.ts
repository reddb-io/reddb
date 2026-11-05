import type { ImageAsset, SectionHeadingLevel } from "@reddb-io/design-system/base";

/** Portable ProductOverview properties shared by native adapters. */
export interface ProductOverviewContract {
/** Heading text for the product; it is also the section's accessible name. */
    title: string;
/** Supporting line under the title. Nothing is rendered when omitted. */
    description?: string;
/** Caller-formatted price, shown under the heading. Nothing is rendered when omitted. */
    price?: string;
/** Outline depth of the heading, `1` to `6`. Defaults to `2`. */
    level?: SectionHeadingLevel;
/**
     * The product imagery as an image the app's pipeline produced, rendered through the Base Image; ignored
     * when a `media` snippet is given.
     */
    image?: ImageAsset;
/** The `image` is the page's Largest Contentful Paint: fetched eagerly at high priority. */
    priority?: boolean;
}
