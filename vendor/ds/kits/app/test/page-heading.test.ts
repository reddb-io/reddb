import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import PageHeading from "../src/composites/PageHeading.svelte";
import { pageHeading } from "../src/composites/page-heading.variants";
import HeadingContractFailures from "./fixtures/HeadingContractFailures.svelte";
import TitlingSurfacesConsumer from "./fixtures/TitlingSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const actions = createRawSnippet(() => ({
  render: () => '<button type="button" data-primary-action>New deployment</button>',
}));

describe("the deliberately failing page heading fixture", () => {
  it("breaks the document outline by starting the page at h2", () => {
    const root = render(HeadingContractFailures);
    const headings = [...root.querySelectorAll("h1, h2, h3, h4, h5, h6")];

    expect(headings.map((heading) => heading.tagName)).toEqual(["H2", "H3"]);
    expect(root.querySelector("h1")).toBeNull();
  });
});

describe("PageHeading", () => {
  it("owns the page title as exactly one h1 and keeps subordinate content caller-owned", () => {
    const element = rendered(
      render(PageHeading, {
        title: "Deployments",
        description: "Manage releases across environments.",
        actions,
      }),
    );

    expect(element.querySelectorAll("h1")).toHaveLength(1);
    expect(element.querySelector("h1")?.textContent).toBe("Deployments");
    expect(element.querySelector("h2, h3, h4, h5, h6")).toBeNull();
    expect(element.querySelector("p")?.textContent).toBe("Manage releases across environments.");
  });

  it("preserves native action focus order", () => {
    const secondary = createRawSnippet(() => ({
      render: () =>
        '<span><a href="/help" data-first-action>Help</a><button type="button" data-last-action>Create</button></span>',
    }));
    const element = rendered(render(PageHeading, { title: "Deployments", actions: secondary }));
    const controls = [...element.querySelectorAll<HTMLElement>("a, button")];

    expect(controls.map((control) => control.textContent)).toEqual(["Help", "Create"]);
    controls[0]!.focus();
    expect(document.activeElement).toBe(controls[0]);
    controls[1]!.focus();
    expect(document.activeElement).toBe(controls[1]);
  });

  it("routes spacing through Density and inherits every nested appearance axis", () => {
    const root = render(TitlingSurfacesConsumer);
    const element = root.querySelector<HTMLElement>("[data-page-heading]")!;

    expect(classes(element)).toEqual(classesOf(pageHeading().root()));
    expect(classes(element).has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
    expect(element.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("wraps actions below the title before squeezing the identity column", () => {
    const element = rendered(render(PageHeading, { title: "Deployments", description: "Every rollout" }));
    const identity = element.querySelector("h1")!.parentElement!;

    expect(classes(element).has("flex-wrap")).toBe(true);
    expect(classes(identity).has("flex-[1_1_20rem]")).toBe(true);
    expect(classes(identity).has("flex-1")).toBe(false);
  });

  it("inherits the page ground while retaining its structural boundary", () => {
    const element = rendered(render(PageHeading, { title: "Deployments" }));
    const contract = classes(element);

    expect(contract.has("bg-transparent")).toBe(true);
    expect(contract.has("bg-elevation-sunken-surface")).toBe(false);
    expect(contract.has("border-b")).toBe(true);
    expect(contract.has("border-elevation-sunken-border")).toBe(true);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("page-heading", "PageHeading");
  });
});
