/** Pure byte normalization and aligned row formatting for the Hex viewer. */

export type HexInput = ArrayBuffer | Uint8Array | readonly number[];

export interface HexRow {
  offset: string;
  hex: string;
  ascii: string;
}

/** Copy binary input into an immutable byte sequence for rendering. */
export function hexBytes(value: HexInput): Uint8Array {
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (value instanceof Uint8Array) return value.slice();
  for (const byte of value) {
    if (!Number.isInteger(byte) || byte < 0 || byte > 0xff) {
      throw new RangeError(`Hex bytes must be integers from 0 to 255; received ${byte}.`);
    }
  }
  return Uint8Array.from(value);
}

/** Format offset, hexadecimal, and printable-ASCII columns at fixed widths. */
export function formatHex(value: HexInput, bytesPerRow = 16): readonly HexRow[] {
  if (!Number.isInteger(bytesPerRow) || bytesPerRow < 1) {
    throw new RangeError("bytesPerRow must be a positive integer.");
  }

  const bytes = hexBytes(value);
  const offsetWidth = Math.max(8, Math.max(0, bytes.length - 1).toString(16).length);
  const hexWidth = bytesPerRow * 3 - 1;
  const rows: HexRow[] = [];

  for (let offset = 0; offset < bytes.length; offset += bytesPerRow) {
    const row = bytes.slice(offset, offset + bytesPerRow);
    const hex = [...row]
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join(" ")
      .padEnd(hexWidth, " ");
    const ascii = [...row]
      .map((byte) => (byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : "."))
      .join("")
      .padEnd(bytesPerRow, " ");

    rows.push({
      offset: offset.toString(16).padStart(offsetWidth, "0"),
      hex,
      ascii,
    });
  }

  return rows;
}
