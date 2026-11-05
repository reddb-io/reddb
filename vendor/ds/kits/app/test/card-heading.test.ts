import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CardHeading from "../src/composites/CardHeading.svelte";
import { cardHeading } from "../src/composites/card-heading.variants";
import TitlingSurfacesConsumer from "./fixtures/TitlingSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

describe("CardHeading", () => {
  it("composes the canonical section-heading contract at the caller's outline level", () => {
    const element = rendered(
      render(CardHeading, {
        title: "Environment",
        description: "Production settings",
        level: 4,
      }),
    );

    expect(element.matches("[data-section-heading][data-card-heading]")).toBe(true);
    expect(element.querySelector("h4")?.textContent).toBe("Environment");
    expect(element.querySelector("p")?.textContent).toBe("Production settings");
    for (const className of classesOf(cardHeading())) {
      expect(classes(element).has(className)).toBe(true);
    }
  });

  it("leaves its action cluster's native keyboard behavior intact", () => {
    const actions = createRawSnippet(() => ({
      render: () => '<button type="button">Edit environment</button>',
    }));
    const element = rendered(render(CardHeading, { title: "Environment", actions }));
    const button = element.querySelector<HTMLButtonElement>("button")!;

    button.focus();
    expect(document.activeElement).toBe(button);
  });

  it("uses Density roles and inherits nested appearance", () => {
    const root = render(TitlingSurfacesConsumer);
    const element = root.querySelector<HTMLElement>("[data-card-heading]")!;

    expect(classes(element).has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
    expect(element.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("card-heading", "CardHeading");
  });
});
