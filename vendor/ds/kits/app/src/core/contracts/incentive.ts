import type { SectionHeadingLevel } from "@reddb-io/design-system/base";

/** Portable Incentive properties shared by native adapters. */
export interface IncentiveContract {
/** Heading text for the proposition; it is also the region's accessible name. */
    title: string;
/** Supporting line under the title. Nothing is rendered when omitted. */
    description?: string;
/** Outline depth of the heading, `1` to `6`. Defaults to `2`. */
    level?: SectionHeadingLevel;
}
