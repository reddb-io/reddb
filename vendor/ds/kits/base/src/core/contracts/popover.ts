type Side = "top" | "right" | "bottom" | "left";
type Align = "start" | "center" | "end";
import type { ButtonSize, ButtonVariant } from "../../button.variants";

/** Canonical portable Popover properties, shared by native adapters. */
export interface PopoverContract {
/** Accessible name for the Button that opens the Popover. */
    triggerLabel: string;
/** Accessible name for the opened non-modal dialog. */
    contentLabel: string;
/** Whether the Popover is open. Bindable for controlled consumers. */
    open?: boolean;
/** Disables the canonical trigger Button. */
    disabled?: boolean;
/** Button emphasis for the trigger. */
    variant?: ButtonVariant;
/** Button size for the trigger. */
    size?: ButtonSize;
/** Preferred side of the trigger; collision handling may flip it. */
    side?: Side;
/** Preferred alignment along the selected side. */
    align?: Align;
/** Distance from the trigger in pixels. */
    sideOffset?: number;
/** Safe distance from viewport collision edges in pixels. */
    collisionPadding?: number;
/** Extra classes merged onto the canonical trigger Button. */
    triggerClass?: string;
}
