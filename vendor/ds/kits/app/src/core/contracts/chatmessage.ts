

/** Portable ChatMessage properties shared by native adapters. */
export interface ChatMessageContract {
/** Visible and accessible author name. */
    author: string;
/**
     * Image source for the author's avatar. When omitted or unloadable, `avatarFallback` is shown
     * instead.
     */
    avatarSrc?: string;
/** Visible fallback when the author's image is unavailable. */
    avatarFallback: string;
/** Machine-readable message timestamp. */
    datetime: string;
/** Caller-owned visible timestamp. */
    time: string;
}
