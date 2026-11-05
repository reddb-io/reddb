

/** Portable Review properties shared by native adapters. */
export interface ReviewContract {
/** Visible and accessible reviewer name. */
    author: string;
/**
     * Image source for the reviewer's avatar. When omitted or unloadable, `avatarFallback` is shown
     * instead.
     */
    avatarSrc?: string;
/** Visible fallback when the reviewer's image is unavailable. */
    avatarFallback: string;
/** Read-only score shown through native state, symbols, and text. */
    rating: number;
/** Highest possible score, which sets how many rating stops are drawn. Defaults to `5`. */
    maxRating?: number;
/** Optional machine-readable review date or time. */
    datetime?: string;
/** Caller-owned visible rendering of the review date or time. */
    time?: string;
}
