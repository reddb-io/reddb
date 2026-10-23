import { describe, expect, it } from "vitest";
import { CARD_TONES, DEPRECATED_CARD_TONES, TONES } from "./fixtures/surface-separation-consumer";

describe("the Base Card tone vocabulary", () => {
  it("publishes the shared tone set through the Base Kit entry point", () => {
    // One vocabulary (ADR 0026): Card's tones are TONES, and `brand` — an
    // emphasis, not a tone — stays one release as a deprecated value.
    expect(CARD_TONES).toEqual(TONES);
    expect(CARD_TONES).toEqual(["neutral", "info", "success", "warning", "danger"]);
    expect(DEPRECATED_CARD_TONES).toEqual(["brand"]);
  });
});
