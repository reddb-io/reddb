import type { TimelineItem } from "@reddb-io/design-system/base";

type ActivityFeedItem = TimelineItem;
/** Portable ActivityFeed properties shared by native adapters. */
export interface ActivityFeedContract {
/** Caller-owned activity in chronological display order. */
    items: readonly ActivityFeedItem[];
/** Accessible name for this activity stream. */
    label?: string;
}
