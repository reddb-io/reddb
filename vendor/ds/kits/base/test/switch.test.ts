import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import BinaryChoiceConsumer from "./fixtures/BinaryChoiceConsumer.svelte";
import { Switch, switchControl } from "./fixtures/binary-choice-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function renderedSwitch(props: Record<string, unknown> = {}): HTMLInputElement {
  const root = rendered(render(Switch, { label: "Automatic updates", ...props }));
  return root.querySelector<HTMLInputElement>('input[role="switch"]')!;
}

describe("the Base Switch", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Switch).toBeDefined();
    expect(switchControl).toBeTypeOf("function");
  });

  it("composes Field into a visibly named native switch contract", () => {
    const root = rendered(
      render(Switch, {
        label: "Automatic updates",
        help: "Install stable releases automatically.",
        required: true,
        name: "updates",
      }),
    );
    const element = root.querySelector<HTMLInputElement>('input[role="switch"]')!;
    const label = root.querySelector<HTMLLabelElement>(`label[for="${element.id}"]`);

    expect(element.type).toBe("checkbox");
    expect(element.getAttribute("role")).toBe("switch");
    expect(element.name).toBe("updates");
    expect(element.required).toBe(true);
    expect(label?.textContent).toContain("Automatic updates");
    expect(element.getAttribute("aria-describedby")).not.toBeNull();
  });

  it("keeps native focus, binary state, form value, and change events", () => {
    const onchange = vi.fn();
    const form = rendered(render(BinaryChoiceConsumer, { switchOnchange: onchange }));
    const element = form.querySelector<HTMLInputElement>('input[role="switch"]')!;

    element.focus();
    expect(document.activeElement).toBe(element);
    expect(element.checked).toBe(false);
    expect(new FormData(form as HTMLFormElement).has("updates")).toBe(false);

    element.click();
    flushSync();
    expect(element.checked).toBe(true);
    expect(new FormData(form as HTMLFormElement).get("updates")).toBe("automatic");
    expect(onchange).toHaveBeenCalledTimes(1);
  });

  it("uses position as well as color for state and selects no appearance axis", () => {
    const element = renderedSwitch({ class: "shrink-0" });
    const nested = rendered(render(BinaryChoiceConsumer, {})).querySelector<HTMLInputElement>(
      '[data-appearance-scope] input[role="switch"]',
    )!;

    expect(classes(element)).toEqual(classesOf(switchControl({ class: "shrink-0" })));
    expect(classes(element).has("checked:before:translate-x-[calc(var(--reddb-spatial-switch-track-height)*0.75)]")).toBe(true);
    expect(classes(element).has("focus-visible:outline-focus")).toBe(true);
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
    expect(classes(nested).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
  });
});

describe("the Switch layout", () => {
  it("puts the switch first and its label beside it, with help aligned under the label", () => {
    const root = rendered(render(Switch, { label: "Automatic updates", help: "Applies overnight." }));
    const control = root.querySelector<HTMLInputElement>('input[role="switch"]')!;
    const label = root.querySelector<HTMLLabelElement>(`label[for="${control.id}"]`)!;
    const help = root.querySelector<HTMLElement>(`#${control.id}-help`)!;

    expect(root.getAttribute("data-field-layout")).toBe("inline");
    expect([...root.children].indexOf(control)).toBe(0);
    expect([...root.children].indexOf(label)).toBe(1);
    expect(classes(help).has("col-start-2")).toBe(true);
    expect(control.getAttribute("aria-describedby")).toBe(help.id);
    // A checked switch is an action state and keeps the Brand fill.
    expect(classes(control).has("checked:after:bg-primary")).toBe(true);
  });
});
