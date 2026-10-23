import type { Tone } from "@reddb-io/design-system/base";
import type { NavigationStatus } from "../../composites/navigation.behavior";

/** Portable SidebarPanel properties shared by native adapters. */
export interface SidebarPanelContract {
/** Visible panel heading and accessible region name. */
    title: string;
/** Optional header notification count or short text. Numeric counts above 99 display 99+ while retaining the exact accessible count. */
    badge?: number | string;
/** Complete accessible notification description. */
    badgeLabel?: string;
/** Semantic notification tone. Defaults to neutral. */
    badgeTone?: Tone;
/** Application-owned presence or health shown beside the heading. */
    status?: NavigationStatus;
/** Heading outline depth, independent of its compact visual size. */
    headingLevel?: 2 | 3 | 4 | 5 | 6;
/** Optional contextual back destination. No back control is rendered without it. */
    backHref?: string;
/** Visible name for the contextual back link. */
    backLabel?: string;
}
