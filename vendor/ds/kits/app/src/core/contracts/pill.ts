import type { Tone } from "@reddb-io/design-system/base";
import type { DeprecatedPillVariant, PillSize, PillVariant } from "../../primitives/pill.variants";

/** Portable Pill properties shared by native adapters. */
export interface PillContract {
/** What the Pill means: the shared tone vocabulary (ADR 0026). Defaults to `neutral`. */
    tone?: Tone;
/**
     * How strongly the tone is drawn: `tinted` (default), `filled` or
     * `outline`. `neutral` and `primary` are deprecated for one release.
     */
    variant?: PillVariant | DeprecatedPillVariant;
/** Defaults to `md`. */
    size?: PillSize;
/** Accessible name for the dismiss affordance. */
    dismissLabel?: string;
}
