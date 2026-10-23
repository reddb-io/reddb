type Side = "top" | "right" | "bottom" | "left";
type Align = "start" | "center" | "end";


/** Canonical portable LinkPreview properties, shared by native adapters. */
export interface LinkPreviewContract {
/** Native destination retained whether or not the preview is available. */
    href: string;
/** Visible fallback link text. */
    label: string;
/** Accessible name for the preview dialog. */
    previewLabel: string;
/** Whether the preview is open. Bindable for controlled consumers. */
    open?: boolean;
/** Delay before keyboard focus or pointer hover opens the preview. */
    openDelay?: number;
/** Delay before leaving the trigger and preview closes it. */
    closeDelay?: number;
/** Preferred side of the link; collision handling may flip it. */
    side?: Side;
/** Preferred alignment along the selected side. */
    align?: Align;
/** Distance from the link in pixels. */
    sideOffset?: number;
/** Safe distance from viewport collision edges in pixels. */
    collisionPadding?: number;
/** Optional classes for the native link itself. */
    triggerClass?: string;
}
