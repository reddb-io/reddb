import { describe, expect, it, vi } from "vitest";
import SurfaceControlsConsumer from "./fixtures/SurfaceControlsConsumer.svelte";
import SurfaceControlsContractFailures from "./fixtures/SurfaceControlsContractFailures.svelte";
import {
  SCROLL_AREA_ORIENTATIONS,
  ScrollArea,
  scrollArea,
} from "./fixtures/surface-controls-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately unreachable scroll-area fixture", () => {
  it("demonstrates overflow that keyboard users cannot focus", () => {
    const area = rendered(
      render(SurfaceControlsContractFailures, { failure: "unreachable-scroll-area" }),
    );

    expect(classes(area).has("overflow-y-auto")).toBe(true);
    expect(area.getAttribute("tabindex")).toBeNull();
    area.focus();
    expect(document.activeElement).not.toBe(area);
  });
});

describe("the Base ScrollArea", () => {
  it("names the contained region and makes native scrolling keyboard reachable", () => {
    const onkeydown = vi.fn();
    const area = rendered(
      render(ScrollArea, { label: "Recent deployments", onkeydown }),
    );

    expect(area.getAttribute("role")).toBe("region");
    expect(area.getAttribute("aria-label")).toBe("Recent deployments");
    expect(area.tabIndex).toBe(0);
    area.focus();
    area.dispatchEvent(new KeyboardEvent("keydown", { key: "PageDown", bubbles: true }));
    expect(document.activeElement).toBe(area);
    expect(onkeydown).toHaveBeenCalledTimes(1);
  });

  it("publishes every native overflow direction", () => {
    expect(SCROLL_AREA_ORIENTATIONS).toEqual(["vertical", "horizontal", "both"]);

    for (const orientation of SCROLL_AREA_ORIENTATIONS) {
      const area = rendered(render(ScrollArea, { label: "Logs", orientation }));
      expect(classes(area)).toEqual(classesOf(scrollArea({ orientation })));
    }
  });

  it("uses a live Density inset and inherits every nested appearance axis", () => {
    const area = render(SurfaceControlsConsumer)
      .querySelector<HTMLElement>("[data-scroll-area]")!;

    expect(classes(area).has("p-[var(--reddb-spatial-inset-sm)]")).toBe(true);
    expect(area.hasAttribute("data-theme")).toBe(false);
    expect(area.hasAttribute("data-color-scheme")).toBe(false);
    expect(area.hasAttribute("data-density")).toBe(false);
  });
});
