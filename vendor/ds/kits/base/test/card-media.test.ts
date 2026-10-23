import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  CARD_MEDIA_FITS,
  CARD_MEDIA_POSITIONS,
  CARD_MEDIA_RATIOS,
  CARD_MEDIA_SPANS,
  Card,
} from "./fixtures/surface-separation-consumer";
import { classes, render, rendered } from "./mount";

const image = createRawSnippet(() => ({
  render: () => '<img src="/product.webp" alt="Product" width="1200" height="800" />',
}));

describe("the Base Card media model", () => {
  it("exposes the complete orthogonal media vocabulary", () => {
    expect(CARD_MEDIA_FITS).toEqual(["cover", "contain"]);
    expect(CARD_MEDIA_POSITIONS).toEqual(["center", "top", "bottom", "start", "end"]);
    expect(CARD_MEDIA_RATIOS).toEqual(["16/9", "4/3", "1/1", "auto"]);
    expect(CARD_MEDIA_SPANS).toEqual(["third", "two-fifths", "half", "background"]);
  });

  it("maps fit, focal position, and ratio independently onto the media region", () => {
    const expectedFit = {
      cover: "[&>*]:object-cover",
      contain: "[&>*]:object-contain",
    } as const;
    const expectedPosition = {
      center: "[&>*]:object-center",
      top: "[&>*]:object-top",
      bottom: "[&>*]:object-bottom",
      start: "[&>*]:object-left",
      end: "[&>*]:object-right",
    } as const;
    const expectedRatio = {
      "16/9": "aspect-video",
      "4/3": "aspect-[4/3]",
      "1/1": "aspect-square",
      auto: "aspect-auto",
    } as const;

    for (const fit of CARD_MEDIA_FITS) {
      const media = rendered(render(Card, { media: image, fit })).querySelector(
        "[data-card-media]",
      )!;
      expect(classes(media).has(expectedFit[fit])).toBe(true);
    }
    for (const position of CARD_MEDIA_POSITIONS) {
      const media = rendered(
        render(Card, { media: image, position }),
      ).querySelector("[data-card-media]")!;
      expect(classes(media).has(expectedPosition[position])).toBe(true);
    }
    for (const ratio of CARD_MEDIA_RATIOS) {
      const media = rendered(
        render(Card, { media: image, ratio }),
      ).querySelector("[data-card-media]")!;
      expect(classes(media).has(expectedRatio[ratio])).toBe(true);
    }
  });

  it("gives horizontal media every declared width and vertical media a full-bleed band", () => {
    const widths = {
      third: "@sm/card:w-1/3",
      "two-fifths": "@sm/card:w-2/5",
      half: "@sm/card:w-1/2",
    } as const;

    for (const span of ["third", "two-fifths", "half"] as const) {
      const horizontal = rendered(
        render(Card, { media: image, orientation: "horizontal", span }),
      ).querySelector("[data-card-media]")!;
      const vertical = rendered(
        render(Card, { media: image, orientation: "vertical", span }),
      ).querySelector("[data-card-media]")!;

      expect(classes(horizontal).has(widths[span])).toBe(true);
      expect(classes(vertical).has("w-full")).toBe(true);
    }
  });

  it("layers background media behind content with the BackgroundMedia scrim contract", () => {
    const card = rendered(
      render(Card, {
        media: image,
        span: "background",
        title: "Overlaid title",
      }),
    );
    const media = card.querySelector("[data-card-media]")!;
    const content = card.querySelector("[data-card-content]")!;
    const scrim = card.querySelector("[data-background-media-scrim]")!;

    expect(classes(media).has("absolute")).toBe(true);
    expect(classes(media).has("inset-0")).toBe(true);
    expect(classes(content).has("relative")).toBe(true);
    expect(classes(content).has("z-10")).toBe(true);
    expect(scrim.getAttribute("data-background-media-scrim")).toBe("strong");
  });

  it("preserves media aspect and clips every span to the Card radius", () => {
    for (const span of CARD_MEDIA_SPANS) {
      const card = rendered(render(Card, { media: image, span }));
      const media = card.querySelector("[data-card-media]")!;

      expect(classes(card).has("rounded-lg")).toBe(true);
      expect(classes(card).has("overflow-hidden")).toBe(true);
      expect(classes(media).has("[&>*]:object-cover")).toBe(true);
      expect(classes(media).has("[&>*]:object-fill")).toBe(false);
    }
  });
});
