import type { HexInput } from "../../hex";
/** Portable HexViewer properties shared by native adapters. */
export interface HexViewerContract {
    /**
         * The bytes to inspect: an `ArrayBuffer`, a `Uint8Array` or an array of integers from 0 to 255.
         * An array entry outside that range throws a `RangeError`; empty input shows a "No bytes."
         * message.
         */
    value: HexInput;
    /**
         * How many bytes each row shows. Must be a positive integer, otherwise a `RangeError` is
         * thrown. Defaults to `16`.
         */
    bytesPerRow?: number;
    /** Accessible name for the byte table. */
    label?: string;
}
