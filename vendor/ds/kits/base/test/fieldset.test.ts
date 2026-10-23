import { describe, expect, it } from "vitest";
import FormScaffoldingConsumer from "./fixtures/FormScaffoldingConsumer.svelte";
import FormScaffoldingFailures from "./fixtures/FormScaffoldingFailures.svelte";
import { Fieldset, fieldset } from "./fixtures/form-scaffolding-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function groupingFailures(root: HTMLElement): string[] {
  const intendedGroup = root.querySelector("[data-intended-group]");
  const group = intendedGroup?.closest("fieldset");
  const legend = group?.querySelector(":scope > legend");
  return group && legend?.textContent?.trim() ? [] : ["controls lost fieldset grouping semantics"];
}

describe("the Base Fieldset", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Fieldset).toBeDefined();
    expect(fieldset).toBeTypeOf("function");
  });

  it("is a native named group whose controls keep native focus", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFieldSetElement>("fieldset")!;
    const legend = element.querySelector(":scope > legend");
    const control = element.querySelector<HTMLInputElement>("input")!;

    expect(element.tagName).toBe("FIELDSET");
    expect(legend?.textContent?.trim()).toBe("Contact preferences");
    expect(groupingFailures(root)).toEqual([]);

    control.focus();
    expect(document.activeElement).toBe(control);
  });

  it("forwards native disabled grouping to every contained control", () => {
    const root = rendered(render(FormScaffoldingConsumer, { disabledGroup: true }));
    const element = root.querySelector<HTMLFieldSetElement>("fieldset")!;
    const control = element.querySelector<HTMLInputElement>("input")!;

    expect(element.disabled).toBe(true);
    expect(control.matches(":disabled")).toBe(true);
  });

  it("renders a group's help and error and steps the legend down when the group is disabled", () => {
    const element = rendered(
      render(Fieldset, { legend: "Delivery", id: "delivery", help: "Pick one.", error: "Choose a delivery." }),
    ) as HTMLFieldSetElement;
    const styles = fieldset();
    const help = element.querySelector<HTMLElement>("#delivery-help")!;
    const error = element.querySelector<HTMLElement>("#delivery-error")!;

    expect(help.textContent).toBe("Pick one.");
    expect(classes(help)).toEqual(classesOf(styles.help()));
    expect(error.getAttribute("role")).toBe("alert");
    expect(classes(error)).toEqual(classesOf(styles.error()));
    expect(classes(error).has("text-feedback-danger-foreground")).toBe(true);
    expect(element.getAttribute("data-fieldset-invalid")).toBe("true");
    expect(classes(element).has("group/fieldset")).toBe(true);
    expect(classes(element.querySelector("legend")!).has("group-disabled/fieldset:text-ink-muted")).toBe(true);

    const quiet = rendered(render(Fieldset, { legend: "Delivery" }));
    expect(quiet.querySelector("p")).toBeNull();
    expect(quiet.hasAttribute("data-fieldset-invalid")).toBe(false);
  });

  it("wears exactly its exported appearance and merges consumer classes", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFieldSetElement>("fieldset")!;
    const styles = fieldset();

    expect(classes(element)).toEqual(classesOf(styles.root({ class: "min-w-0" })));
    expect(classes(element.querySelector("legend")!)).toEqual(classesOf(styles.legend()));
  });

  it("keeps Density and nested appearance selection outside the component", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFieldSetElement>(
      "[data-appearance-scope] fieldset",
    )!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    // Field to field inside a group is the layout tier's smaller step (ADR 0024).
    expect(classes(element).has("gap-[var(--reddb-spatial-layout-gap-sm)]")).toBe(true);
  });
});

describe("the deliberately failing Fieldset fixture", () => {
  it("diagnoses controls whose visual group lost native semantics", () => {
    const root = rendered(
      render(FormScaffoldingFailures, { failure: "lost-fieldset-group" }),
    );
    expect(groupingFailures(root)).toEqual(["controls lost fieldset grouping semantics"]);
  });
});
