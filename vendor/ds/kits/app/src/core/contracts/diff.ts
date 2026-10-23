import type { DiffRow } from "../diff";
import type { DiffMode } from "../../composites/diff.variants";

/** Portable Diff properties shared by native adapters. */
export interface DiffContract {
/** Accessible name for this comparison. */
    label?: string;
/** Caller-computed lines in display order. */
    rows: readonly DiffRow[];
/** Unified reading order or aligned resizable panes. */
    mode?: DiffMode;
/** Visible and accessible name for the old side. */
    beforeLabel?: string;
/** Visible and accessible name for the new side. */
    afterLabel?: string;
/** Accessible name for the side-by-side resize control. */
    separatorLabel?: string;
}
