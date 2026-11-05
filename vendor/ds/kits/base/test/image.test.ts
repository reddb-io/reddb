import { describe, expect, it } from "vitest";
import { IMAGE_FITS, IMAGE_POSITIONS, Image } from "../src/index";
import { classes, render } from "./mount";

const img = (target: HTMLElement) => target.querySelector("img")!;

describe("the Base Image", () => {
  it("loads lazily and decodes asynchronously by default, reserving its intrinsic box", () => {
    const element = img(render(Image, { src: "/a.webp", alt: "A chart", width: 1600, height: 900 }));
    expect(element.getAttribute("alt")).toBe("A chart");
    expect(element.getAttribute("width")).toBe("1600");
    expect(element.getAttribute("height")).toBe("900");
    expect(element.getAttribute("loading")).toBe("lazy");
    expect(element.getAttribute("decoding")).toBe("async");
    expect(element.hasAttribute("fetchpriority")).toBe(false);
    expect(Number.parseFloat(element.style.aspectRatio)).toBeCloseTo(1600 / 900);
    expect(classes(element).has("h-auto")).toBe(true);
    expect(classes(element).has("max-w-full")).toBe(true);
  });

  it("marks the priority image for high-priority, eager, synchronous loading", () => {
    const element = img(
      render(Image, { src: "/hero.webp", alt: "Hero", width: 1200, height: 675, priority: true }),
    );
    expect(element.getAttribute("fetchpriority")).toBe("high");
    expect(element.getAttribute("loading")).toBe("eager");
    expect(element.getAttribute("decoding")).toBe("sync");
    expect(element.hasAttribute("data-image-priority")).toBe(true);
  });

  it("reserves a caller ratio when the intrinsic size is unknown", () => {
    const element = img(render(Image, { src: "/a.webp", alt: "A", ratio: 4 / 3 }));
    expect(element.hasAttribute("width")).toBe(false);
    expect(Number.parseFloat(element.style.aspectRatio)).toBeCloseTo(4 / 3);
  });

  it("refuses an unsized image", () => {
    expect(() => render(Image, { src: "/a.webp", alt: "A" })).toThrow(/width and height.*or ratio/);
    expect(() => render(Image, { src: "/a.webp", alt: "A", width: 100 })).toThrow(/ratio/);
  });

  it("refuses an empty alternative unless the image is declared decorative", () => {
    expect(() => render(Image, { src: "/a.webp", alt: "", ratio: 1 })).toThrow(/decorative/);
    expect(() => render(Image, { src: "/a.webp", alt: "   ", ratio: 1 })).toThrow(/decorative/);
    expect(() => render(Image, { src: "/a.webp", alt: "A", decorative: true, ratio: 1 })).toThrow(
      /decorative/,
    );
    const element = img(render(Image, { src: "/a.webp", alt: "", decorative: true, ratio: 1 }));
    expect(element.getAttribute("alt")).toBe("");
  });

  it("passes srcset and sizes through, and renders picture sources in order", () => {
    const target = render(Image, {
      src: "/a-960.jpg",
      alt: "A",
      width: 960,
      height: 540,
      srcset: "/a-480.jpg 480w, /a-960.jpg 960w",
      sizes: "(min-width: 48rem) 50vw, 100vw",
      sources: [
        { srcset: "/a-480.avif 480w, /a-960.avif 960w", type: "image/avif" },
        { srcset: "/a-480.webp 480w, /a-960.webp 960w", type: "image/webp" },
      ],
    });
    const picture = target.querySelector("picture")!;
    expect(picture).not.toBeNull();
    expect(classes(picture).has("contents")).toBe(true);
    const sources = [...picture.querySelectorAll("source")];
    expect(sources.map((source) => source.getAttribute("type"))).toEqual(["image/avif", "image/webp"]);
    expect(sources[0]!.getAttribute("sizes")).toBe("(min-width: 48rem) 50vw, 100vw");
    const element = img(target);
    expect(element.parentElement).toBe(picture);
    expect(element.getAttribute("srcset")).toBe("/a-480.jpg 480w, /a-960.jpg 960w");
    expect(element.getAttribute("sizes")).toBe("(min-width: 48rem) 50vw, 100vw");
  });

  it("takes fit and focal position from the named vocabulary", () => {
    for (const fit of IMAGE_FITS) {
      const element = img(render(Image, { src: "/a.webp", alt: "A", ratio: 1, fit }));
      expect(classes(element).has(`object-${fit}`)).toBe(true);
    }
    const physical = { center: "center", top: "top", bottom: "bottom", start: "left", end: "right" };
    for (const position of IMAGE_POSITIONS) {
      const element = img(render(Image, { src: "/a.webp", alt: "A", ratio: 1, position }));
      expect(classes(element).has(`object-${physical[position]}`)).toBe(true);
    }
  });
});

describe("Base composites taking an image", () => {
  const asset = { src: "/card.webp", alt: "Card art", width: 1200, height: 800 };

  it("renders a Card image inside the media region at the Card's ratio, fit and position", async () => {
    const { Card } = await import("../src/index");
    const target = render(Card, { title: "A", image: asset, fit: "contain", position: "top", priority: true });
    const element = img(target);
    expect(element.closest("[data-card-media]")).not.toBeNull();
    expect(Number.parseFloat(element.style.aspectRatio)).toBeCloseTo(16 / 9);
    expect(classes(element).has("object-contain")).toBe(true);
    expect(classes(element).has("object-top")).toBe(true);
    expect(element.getAttribute("fetchpriority")).toBe("high");
  });

  it("prioritises only the Carousel slide shown first", async () => {
    const { Carousel } = await import("../src/index");
    const target = render(Carousel, {
      label: "Wallpapers",
      priority: true,
      initialIndex: 1,
      slides: [
        { label: "One", image: { ...asset, src: "/1.webp" } },
        { label: "Two", image: { ...asset, src: "/2.webp" } },
        { label: "Three", image: { ...asset, src: "/3.webp" } },
      ],
    });
    const images = [...target.querySelectorAll("img")];
    expect(images.map((element) => element.getAttribute("loading"))).toEqual(["lazy", "eager", "lazy"]);
    expect(images[1]!.getAttribute("fetchpriority")).toBe("high");
  });
});
