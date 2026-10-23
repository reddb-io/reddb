import { describe, expect, it } from "vitest";
import ContentMaskContractFailures from "./fixtures/ContentMaskContractFailures.svelte";
import { render, rendered } from "./mount";

describe("the deliberately failing ContentMask fixtures", () => {
  it("demonstrates a mask that drops its image's accessible name", () => {
    const mask = rendered(render(ContentMaskContractFailures, { failure: "hidden-name" }));
    const image = mask.querySelector<HTMLImageElement>("img")!;

    expect(image.alt).toBe("Ada Lovelace");
    expect(image.closest('[aria-hidden="true"]')).toBe(mask);
  });

  it("demonstrates a collapsed clip that leaves its control unreachable", () => {
    const mask = rendered(
      render(ContentMaskContractFailures, { failure: "unreachable-control" }),
    );
    const control = mask.querySelector<HTMLButtonElement>("button")!;

    expect(mask.style.clipPath).toBe("circle(0%)");
    expect(control.disabled).toBe(true);
    expect(control.tabIndex).toBe(-1);
  });
});
