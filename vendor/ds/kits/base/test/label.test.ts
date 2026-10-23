import { describe, expect, it } from "vitest";
import FormScaffoldingConsumer from "./fixtures/FormScaffoldingConsumer.svelte";
import FormScaffoldingFailures from "./fixtures/FormScaffoldingFailures.svelte";
import { Label, label } from "./fixtures/form-scaffolding-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function labelFailures(root: HTMLElement): string[] {
  const failures: string[] = [];
  for (const control of root.querySelectorAll<HTMLInputElement>("input, select, textarea")) {
    const associated =
      control.hasAttribute("aria-label") ||
      (control.id !== "" && root.querySelector(`label[for="${control.id}"]`) !== null);
    if (!associated) failures.push("control has no accessible label");
  }
  return failures;
}

describe("the Base Label", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Label).toBeDefined();
    expect(label).toBeTypeOf("function");
  });

  it("is a native label associated with its control", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLLabelElement>('[data-testid="display-name-label"]')!;
    const control = root.querySelector<HTMLInputElement>("#display-name")!;

    expect(element.tagName).toBe("LABEL");
    expect(element.htmlFor).toBe(control.id);
    expect(element.textContent?.trim()).toBe("Display name");
    expect(labelFailures(root)).toEqual([]);

    expect(element.control).toBe(control);
    control.focus();
    expect(document.activeElement).toBe(control);
  });

  it("forwards native attributes and consumer classes", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLLabelElement>('[data-testid="display-name-label"]')!;

    expect(element.title).toBe("Your public name");
    expect(classes(element)).toEqual(classesOf(label({ class: "tracking-wide" })));
    expect(classes(element).has("tracking-wide")).toBe(true);
  });

  it("does not select an appearance axis inside a nested appearance scope", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLLabelElement>(
      '[data-appearance-scope] [data-testid="nested-label"]',
    )!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    expect(classes(element)).toEqual(classesOf(label()));
  });
});

describe("the deliberately failing Label fixture", () => {
  it("diagnoses an unlabelled control", () => {
    const root = rendered(
      render(FormScaffoldingFailures, { failure: "unlabelled-control" }),
    );
    expect(labelFailures(root)).toEqual(["control has no accessible label"]);
  });
});
