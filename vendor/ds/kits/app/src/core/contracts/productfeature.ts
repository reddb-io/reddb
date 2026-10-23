import type { SectionHeadingLevel } from "@reddb-io/design-system/base";
import type { ProductFeatureMediaSide } from "../../composites/product-feature.variants";

/** Portable ProductFeature properties shared by native adapters. */
export interface ProductFeatureContract {
/** Heading text for the feature; it is also the region's accessible name. */
    title: string;
/** Supporting line under the title. Nothing is rendered when omitted. */
    description?: string;
/** Outline depth of the heading, `1` to `6`. Defaults to `2`. */
    level?: SectionHeadingLevel;
/** Which side of the content the media sits on: `start` or `end`. Defaults to `start`. */
    mediaSide?: ProductFeatureMediaSide;
}
