import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  MediaObject,
  mediaObject as mediaObjectAppearance,
} from "./fixtures/media-presentation-consumer";
import MediaPresentationConsumer from "./fixtures/MediaPresentationConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const text = (value: string) =>
  createRawSnippet(() => ({ render: () => `<span>${value}</span>` }));

describe("the Base MediaObject", () => {
  it("keeps caller-owned media and content in one density-responsive relationship", () => {
    const element = rendered(render(MediaObject, {
      media: text("AL"),
      children: text("Ada wrote the first published algorithm."),
      align: "center",
      gap: "lg",
      class: "max-w-lg",
    }));

    expect([...element.children].map((child) => child.textContent)).toEqual([
      "AL",
      "Ada wrote the first published algorithm.",
    ]);
    expect(classes(element)).toEqual(
      classesOf(
        mediaObjectAppearance({ align: "center", gap: "lg" }).root({ class: "max-w-lg" }),
      ),
    );
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("composes Avatar and Carousel inside the nearest appearance scope without taking focus", () => {
    const root = rendered(render(MediaPresentationConsumer, {}));
    const capabilities = root.querySelectorAll<HTMLElement>(
      "[data-avatar], [data-media-object], [data-carousel]",
    );

    expect(root.getAttribute("data-theme")).toBe("marketing");
    expect(root.getAttribute("data-color-scheme")).toBe("dark");
    expect(root.getAttribute("data-density")).toBe("compact");
    expect(capabilities).toHaveLength(3);
    for (const capability of capabilities) {
      expect(capability.hasAttribute("data-theme")).toBe(false);
      expect(capability.hasAttribute("data-color-scheme")).toBe(false);
      expect(capability.hasAttribute("data-density")).toBe(false);
    }

    const biography = root.querySelector<HTMLAnchorElement>("a")!;
    const slideAction = root.querySelector<HTMLButtonElement>("[data-carousel-slide] button")!;
    biography.focus();
    expect(document.activeElement).toBe(biography);
    slideAction.focus();
    expect(document.activeElement).toBe(slideAction);
  });
});
