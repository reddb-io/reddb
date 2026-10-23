import { describe, expect, it } from "vitest";
import MediaPresentationContractFailures from "./fixtures/MediaPresentationContractFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing media presentation fixtures", () => {
  it("demonstrates an Avatar whose failed image has no text fallback", () => {
    const root = render(MediaPresentationContractFailures, {
      failure: "avatar-without-fallback",
    });
    const avatar = root.querySelector<HTMLElement>("[data-broken-avatar]")!;

    expect(avatar.querySelector("img")).not.toBeNull();
    expect(avatar.querySelector("[data-avatar-fallback]")).toBeNull();
  });

  it("demonstrates an auto-rotating Carousel with no pause control", () => {
    const root = render(MediaPresentationContractFailures, {
      failure: "unpausable-carousel",
    });
    const carousel = root.querySelector<HTMLElement>("[data-broken-carousel]")!;

    expect(carousel.hasAttribute("data-auto-rotates")).toBe(true);
    expect(root.querySelector("button[aria-label*='Pause']")).toBeNull();
  });
});
