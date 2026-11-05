

/** Portable PageHeading properties shared by native adapters. */
export interface PageHeadingContract {
/** The page's title, rendered as the page's one level-1 heading. */
    title: string;
/** Supporting line under the title. Nothing is rendered when omitted. */
    description?: string;
}
