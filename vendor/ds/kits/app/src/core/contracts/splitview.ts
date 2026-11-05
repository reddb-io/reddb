import type { SplitOrientation } from "../../primitives/split-view.behavior";

/** Portable SplitView properties shared by native adapters. */
export interface SplitViewContract {
/** Axis the panes are laid out along. Defaults to `horizontal`. */
    orientation?: SplitOrientation;
/** How much of the container the start pane takes, 0–1. Bindable. */
    fraction?: number;
/** Smallest fraction the start pane may shrink to. */
    min?: number;
/** Largest fraction it may grow to. */
    max?: number;
/** Accessible name for the divider. */
    label?: string;
}
