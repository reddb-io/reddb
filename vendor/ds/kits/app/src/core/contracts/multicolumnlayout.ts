import type { StackGap } from "@reddb-io/design-system/base";

/** Portable MultiColumnLayout properties shared by native adapters. */
export interface MultiColumnLayoutContract {
/** Vertical rhythm inside every column. Defaults to the layout tier's `layout-md`. */
    gap?: StackGap;
}
