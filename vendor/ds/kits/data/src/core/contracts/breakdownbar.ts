import type { ChartNumberFormat } from "../../chart";
import type { BreakdownItem } from "../breakdown-bar";
/** Portable BreakdownBar properties shared by native adapters. */
export interface BreakdownBarContract {
    /** Accessible name for the composition and its group controls. */
    label: string;
    /** Non-negative caller-owned contributions; this component never calculates billing. */
    items: readonly BreakdownItem[];
    /** Formats contributions in the group controls. */
    format?: ChartNumberFormat;
    /** Selected group id, or null for all groups. Bindable; selection never changes the total or segment widths. */
    selected?: string | null;
}
