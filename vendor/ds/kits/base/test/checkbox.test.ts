import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import BinaryChoiceConsumer from "./fixtures/BinaryChoiceConsumer.svelte";
import BinaryChoiceContractFailures from "./fixtures/BinaryChoiceContractFailures.svelte";
import { Checkbox, checkbox } from "./fixtures/binary-choice-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function renderedCheckbox(props: Record<string, unknown> = {}): HTMLInputElement {
  const root = rendered(render(Checkbox, { label: "Share diagnostics", ...props }));
  return root.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
}

function checkboxNamingFailures(root: HTMLElement): string[] {
  const control = root.querySelector<HTMLInputElement>('input[type="checkbox"]');
  const label = control?.id
    ? root.querySelector<HTMLLabelElement>(`label[for="${control.id}"]`)
    : control?.closest("label");
  return control && (label?.textContent?.trim() || control.getAttribute("aria-label"))
    ? []
    : ["checkbox has no accessible label"];
}

describe("the Base Checkbox", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Checkbox).toBeDefined();
    expect(checkbox).toBeTypeOf("function");
  });

  it("composes Field into a visible native checkbox contract", () => {
    const root = rendered(
      render(Checkbox, {
        label: "Share diagnostics",
        help: "Includes anonymous performance measurements.",
        error: "Choose whether diagnostics may be shared.",
        required: true,
        name: "telemetry",
      }),
    );
    const element = root.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    const label = root.querySelector<HTMLLabelElement>(`label[for="${element.id}"]`);

    expect(element.type).toBe("checkbox");
    expect(element.name).toBe("telemetry");
    expect(element.required).toBe(true);
    expect(label?.textContent).toContain("Share diagnostics");
    expect(element.getAttribute("aria-describedby")?.split(" ")).toHaveLength(2);
    expect(element.getAttribute("aria-invalid")).toBe("true");
    expect(checkboxNamingFailures(root)).toEqual([]);
  });

  it("puts the control first and its label beside it, with help aligned under the label", () => {
    const root = rendered(
      render(Checkbox, {
        label: "Share diagnostics",
        help: "Includes anonymous performance measurements.",
        error: "Choose whether diagnostics may be shared.",
      }),
    );
    const element = root.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    const label = root.querySelector<HTMLLabelElement>(`label[for="${element.id}"]`)!;
    const help = root.querySelector<HTMLElement>(`#${element.id}-help`)!;
    const error = root.querySelector<HTMLElement>(`#${element.id}-error`)!;
    const order = [...root.children];

    expect(root.getAttribute("data-field-layout")).toBe("inline");
    // Control first, label after it, on one two-column row.
    expect(order.indexOf(element.closest("[data-choice-control]")!)).toBe(0);
    expect(order.indexOf(label)).toBe(1);
    expect(classes(root).has("grid-cols-[auto_minmax(0,1fr)]")).toBe(true);
    expect(classes(root).has("items-center")).toBe(true);
    // The label stays a clickable native label.
    expect(classes(label).has("cursor-pointer")).toBe(true);
    // Help and error wrap underneath, aligned with the label text.
    expect(classes(help).has("col-start-2")).toBe(true);
    expect(classes(error).has("col-start-2")).toBe(true);
    // The association contract is unchanged by the layout.
    expect(element.getAttribute("aria-describedby")).toBe(`${help.id} ${error.id}`);
    expect(element.getAttribute("aria-invalid")).toBe("true");
    expect(element.getAttribute("aria-errormessage")).toBe(error.id);

    label.click();
    expect(element.checked).toBe(true);
  });

  it("keeps native focus, checked state, form value, and change events", () => {
    const onchange = vi.fn();
    const form = rendered(render(BinaryChoiceConsumer, { checkboxOnchange: onchange }));
    const element = form.querySelector<HTMLInputElement>('input[type="checkbox"]')!;

    element.focus();
    expect(document.activeElement).toBe(element);
    expect(element.checked).toBe(true);
    expect(new FormData(form as HTMLFormElement).get("telemetry")).toBe("allowed");

    element.checked = false;
    element.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    expect(onchange).toHaveBeenCalledTimes(1);
    expect(new FormData(form as HTMLFormElement).has("telemetry")).toBe(false);
  });

  it("keeps the native input as a control-height pointer target and draws the box beside it", () => {
    const element = renderedCheckbox({ error: "Choose one." });
    const styles = checkbox();
    const choice = element.parentElement!;
    const [box, mark] = [element.nextElementSibling!, element.nextElementSibling!.nextElementSibling!];

    // The input is the only control: the drawing is hidden and inert.
    expect(choice.matches("[data-choice-control]")).toBe(true);
    expect(classes(choice)).toEqual(classesOf(styles.root()));
    expect(classes(choice).has("size-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(classes(element).has("appearance-none")).toBe(true);
    expect(classes(element).has("size-full")).toBe(true);
    expect(classes(box)).toEqual(classesOf(styles.box()));
    expect(classes(mark)).toEqual(classesOf(styles.mark()));
    for (const drawn of [box, mark]) {
      expect(drawn.getAttribute("aria-hidden")).toBe("true");
      expect(classes(drawn).has("pointer-events-none")).toBe(true);
    }
    // The box is a glyph: the icon role sizes it, not the control height.
    expect(classes(box).has("size-[var(--reddb-spatial-icon-size-md)]")).toBe(true);
    // Every state the input holds reaches the drawing through `peer-*`.
    expect(classes(element).has("peer")).toBe(true);
    expect(classes(box).has("peer-aria-invalid:border-feedback-danger-border")).toBe(true);
    expect(classes(box).has("peer-checked:bg-primary")).toBe(true);
    expect(classes(box).has("peer-focus-visible:outline-focus")).toBe(true);
    expect(classes(mark).has("peer-checked:visible")).toBe(true);
    expect(classes(choice).has("has-disabled:opacity-50")).toBe(true);
    // A mixed checkbox keeps the native dash: the fill, and the mark laid flat.
    expect(classes(box).has("peer-indeterminate:bg-primary")).toBe(true);
    expect(classes(mark).has("peer-indeterminate:visible")).toBe(true);
    expect(classes(mark).has("peer-indeterminate:rotate-0")).toBe(true);
    // Forced colors hand the control back to the platform, focus outline and all.
    expect(classes(element).has("forced-colors:appearance-auto")).toBe(true);
    expect(classes(element).has("focus-visible:outline-hidden")).toBe(true);
    expect(classes(box).has("forced-colors:hidden")).toBe(true);
  });

  it("wears its exported appearance without selecting an appearance axis", () => {
    const element = renderedCheckbox({ class: "shrink-0" });
    const nested = rendered(render(BinaryChoiceConsumer, {})).querySelector<HTMLInputElement>(
      '[data-appearance-scope] input[type="checkbox"]:not([role])',
    )!;

    expect(classes(element)).toEqual(classesOf(checkbox().control({ class: "shrink-0" })));
    expect(classes(element).has("shrink-0")).toBe(true);
    expect(classes(element).has("focus-visible:outline-hidden")).toBe(true);
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
    expect(classes(nested.parentElement!).has("size-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
  });
});

describe("the deliberately failing checkbox fixture", () => {
  it("diagnoses a checkbox without an accessible label", () => {
    const root = rendered(
      render(BinaryChoiceContractFailures, { failure: "unlabelled-checkbox" }),
    );
    expect(checkboxNamingFailures(root)).toEqual(["checkbox has no accessible label"]);
  });
});
