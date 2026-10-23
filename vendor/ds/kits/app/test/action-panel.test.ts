import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ActionPanel from "../src/composites/ActionPanel.svelte";
import { actionPanel } from "../src/composites/action-panel.variants";
import TitlingSurfacesConsumer from "./fixtures/TitlingSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const content = createRawSnippet(() => ({ render: () => "<p>Deployment configuration</p>" }));
const actions = createRawSnippet(() => ({
  render: () =>
    '<span><button type="button" data-cancel>Cancel</button><button type="button" data-save>Save</button></span>',
}));

describe("ActionPanel", () => {
  it("composes one canonical Card with a section heading, body, and associated actions", () => {
    const panel = rendered(
      render(ActionPanel, {
        title: "Production deployment",
        description: "Review before saving.",
        children: content,
        actions,
      }),
    );

    expect(panel.matches("[data-card][data-action-panel]")).toBe(true);
    expect(panel.getAttribute("role")).toBe("region");
    expect(panel.getAttribute("aria-label")).toBe("Production deployment");
    expect(panel.querySelector("[data-section-heading] h2")?.textContent).toBe("Production deployment");
    expect(panel.querySelector("[data-card-body]")?.textContent).toContain("Deployment configuration");
    expect(panel.querySelectorAll("[data-card-footer] button")).toHaveLength(2);
  });

  it("preserves native action order and focus", () => {
    const panel = rendered(render(ActionPanel, { title: "Deployment", actions }));
    const controls = [...panel.querySelectorAll<HTMLButtonElement>("button")];

    expect(controls.map((control) => control.textContent)).toEqual(["Cancel", "Save"]);
    controls[0]!.focus();
    expect(document.activeElement).toBe(controls[0]);
    controls[1]!.focus();
    expect(document.activeElement).toBe(controls[1]);
  });

  it("routes panel rhythm through Density and inherits nested appearance", () => {
    const root = render(TitlingSurfacesConsumer);
    const panel = root.querySelector<HTMLElement>("[data-action-panel]")!;

    expect(classes(panel)).toEqual(expect.objectContaining(classesOf(actionPanel().root())));
    expect(classes(panel.querySelector("[data-action-panel-content]")!).has(
      "gap-[var(--reddb-spatial-gap-md)]",
    )).toBe(true);
    expect(panel.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(panel.hasAttribute("data-theme")).toBe(false);
    expect(panel.hasAttribute("data-color-scheme")).toBe(false);
    expect(panel.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("action-panel", "ActionPanel");
  });
});
