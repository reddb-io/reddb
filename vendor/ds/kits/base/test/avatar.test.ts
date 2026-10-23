import { flushSync, type ComponentProps } from "svelte";
import { describe, expect, expectTypeOf, it } from "vitest";
import {
  AVATAR_SIZES,
  Avatar,
  avatar as avatarAppearance,
} from "./fixtures/media-presentation-consumer";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Avatar", () => {
  it("requires text only when no icon alternative is supplied", () => {
    type Props = ComponentProps<typeof Avatar>;
    type MissingAlternativeIsAccepted = { name: string } extends Props ? true : false;
    type IconOnlyIsAccepted = { name: string; icon: typeof TestGlyph } extends Props ? true : false;

    expectTypeOf<MissingAlternativeIsAccepted>().toEqualTypeOf<false>();
    expectTypeOf<IconOnlyIsAccepted>().toEqualTypeOf<true>();
  });

  it("renders an icon-only entity alternative inside every named size", () => {
    for (const size of AVATAR_SIZES) {
      const element = rendered(render(Avatar, {
        name: "Build service",
        icon: TestGlyph,
        size,
      }));
      const icon = element.querySelector<SVGElement>("[data-icon]")!;

      expect(element.getAttribute("role")).toBe("img");
      expect(element.getAttribute("aria-label")).toBe("Build service");
      expect(icon.getAttribute("aria-hidden")).toBe("true");
      expect(icon.getAttribute("width")).toBe(`var(--reddb-spatial-icon-size-${size})`);
      expect(element.querySelector("[data-avatar-fallback]")?.contains(icon)).toBe(true);
    }
  });

  it("keeps a required text fallback behind an optional image", () => {
    expect(AVATAR_SIZES).toEqual(["sm", "md", "lg"]);

    const element = rendered(render(Avatar, {
      src: "/ada.png",
      name: "Ada Lovelace",
      fallback: "AL",
      size: "lg",
      class: "ring-offset-1",
    }));

    expect(element.getAttribute("role")).toBe("img");
    expect(element.getAttribute("aria-label")).toBe("Ada Lovelace");
    expect(element.querySelector("img")?.getAttribute("alt")).toBe("");
    expect(element.querySelector("[data-avatar-fallback]")?.textContent).toBe("AL");
    expect(classes(element)).toEqual(
      classesOf(avatarAppearance({ size: "lg" }).root({ class: "ring-offset-1" })),
    );
  });

  it("falls back from an image to its icon alternative without changing its name", () => {
    const element = rendered(render(Avatar, {
      src: "/service.png",
      name: "Build service",
      icon: TestGlyph,
    }));
    const image = element.querySelector<HTMLImageElement>("[data-avatar-image]")!;

    expect(element.querySelector("[data-avatar-icon]")).not.toBeNull();
    expect(image).not.toBeNull();
    image.dispatchEvent(new Event("error"));
    flushSync();

    expect(element.querySelector("[data-avatar-image]")).toBeNull();
    expect(element.querySelector("[data-avatar-icon]")).not.toBeNull();
    expect(element.getAttribute("aria-label")).toBe("Build service");
  });
});
