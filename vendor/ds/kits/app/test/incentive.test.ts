import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import Incentive from "../src/composites/Incentive.svelte";
import { incentive } from "../src/composites/incentive.variants";
import MerchandisingSurfacesConsumer from "./fixtures/MerchandisingSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

describe("Incentive", () => {
  it("composes canonical MediaObject and SectionHeading contracts into a named value proposition", () => {
    const icon = createRawSnippet(() => ({
      render: () => '<svg viewBox="0 0 24 24"><path d="M4 12h16" /></svg>',
    }));
    const incentive = rendered(
      render(Incentive, {
        title: "Complimentary delivery",
        description: "Orders over $50 ship free.",
        level: 3,
        icon,
      }),
    );
    const heading = incentive.querySelector("[data-section-heading] h3")!;

    expect(incentive.matches("[data-media-object][data-incentive][role=region]")).toBe(true);
    expect(incentive.getAttribute("aria-labelledby")).toBe(heading.id);
    expect(heading.textContent).toBe("Complimentary delivery");
    expect(incentive.textContent).toContain("Orders over $50 ship free.");
    expect(incentive.querySelector("[data-incentive-icon]")?.getAttribute("aria-hidden")).toBe(
      "true",
    );
  });

  it("preserves focus for caller-owned incentive actions", () => {
    const root = render(MerchandisingSurfacesConsumer);
    const action = root.querySelector<HTMLAnchorElement>("[data-incentive] a")!;

    action.focus();
    expect(document.activeElement).toBe(action);
  });

  it("routes incentive rhythm through Density and inherits every nested appearance axis", () => {
    const root = render(MerchandisingSurfacesConsumer);
    const surface = root.querySelector<HTMLElement>("[data-incentive]")!;
    const icon = surface.querySelector<HTMLElement>("[data-incentive-icon]")!;

    expect(classes(surface)).toEqual(classesOf(incentive().root()));
    expect(classes(surface)).toContain("gap-[var(--reddb-spatial-gap-md)]");
    expect(classes(icon)).toContain("p-[var(--reddb-spatial-inset-sm)]");
    expect(surface.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(surface.hasAttribute("data-theme")).toBe(false);
    expect(surface.hasAttribute("data-color-scheme")).toBe(false);
    expect(surface.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expect(render(MerchandisingSurfacesConsumer).querySelector("[data-incentive]")).not.toBeNull();
    expectLayoutReadiness("incentive", "Incentive");
  });
});
