

/** Portable CategoryPreview properties shared by native adapters. */
export interface CategoryPreviewContract {
/** Visible category name and the destination's accessible name. */
    name: string;
/** Destination of the link wrapped around `name`. */
    href: string;
/** Supporting line under the category name. Nothing is rendered when omitted. */
    description?: string;
/** Width-to-height ratio of the media region, as a number. Defaults to `4 / 3`. */
    ratio?: number;
/**
     * Outline level of the category name, `2` to `6` (default `3`). Set it one below the
     * heading that introduces the previews so the page outline never skips a level.
     */
    headingLevel?: 2 | 3 | 4 | 5 | 6;
}
