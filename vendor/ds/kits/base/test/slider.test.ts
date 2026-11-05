import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import RangePressedConsumer from "./fixtures/RangePressedConsumer.svelte";
import RangePressedContractFailures from "./fixtures/RangePressedContractFailures.svelte";
import { Slider, slider } from "./fixtures/range-pressed-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function accessibleValueFailures(root: HTMLElement): string[] {
  const selector = 'input[type="range"], [role="slider"]';
  const control = root.matches(selector)
    ? root
    : root.querySelector<HTMLElement>(selector);
  if (!control) return ["slider is missing"];
  if (control instanceof HTMLInputElement && Number.isFinite(control.valueAsNumber)) return [];
  return control.hasAttribute("aria-valuenow") || control.hasAttribute("aria-valuetext")
    ? []
    : ["slider has no accessible value"];
}

describe("the Base Slider", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Slider).toBeDefined();
    expect(slider).toBeTypeOf("function");
  });

  it("composes Field around a named native range with an accessible value", () => {
    const root = rendered(
      render(Slider, {
        label: "Deployment capacity",
        min: 10,
        max: 90,
        step: 10,
        value: 40,
        formatValue: (current: number) => `${current}%`,
      }),
    );
    const control = root.querySelector<HTMLInputElement>('input[type="range"]')!;

    expect(root.querySelector(`label[for="${control.id}"]`)?.textContent).toContain(
      "Deployment capacity",
    );
    expect(control.valueAsNumber).toBe(40);
    expect(control.min).toBe("10");
    expect(control.max).toBe("90");
    expect(control.step).toBe("10");
    expect(control.getAttribute("aria-valuetext")).toBe("40%");
    expect(root.querySelector("output")?.textContent).toBe("40%");
    expect(accessibleValueFailures(root)).toEqual([]);
  });

  it("keeps native focus, keyboard events, live value, and form submission", () => {
    const oninput = vi.fn();
    const form = rendered(render(RangePressedConsumer, { sliderOninput: oninput }));
    const control = form.querySelector<HTMLInputElement>('input[name="capacity"]')!;

    control.focus();
    expect(document.activeElement).toBe(control);
    control.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    control.value = "60";
    control.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();

    expect(oninput).toHaveBeenCalledTimes(1);
    expect(form.querySelector(`output[for="${control.id}"]`)?.textContent).toBe("60%");
    expect(new FormData(form as HTMLFormElement).get("capacity")).toBe("60");
  });

  it("wears token-backed appearance inside a caller-owned nested scope", () => {
    const form = rendered(render(RangePressedConsumer, {}));
    const root = form.querySelector<HTMLElement>("[data-appearance-scope] [data-slider]")!;
    const control = root.querySelector<HTMLInputElement>('input[type="range"]')!;
    const styles = slider();

    expect(classes(control)).toEqual(classesOf(styles.control()));
    expect(classes(control).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});

describe("the deliberately failing Slider fixture", () => {
  it("diagnoses a slider without an accessible value", () => {
    const root = rendered(
      render(RangePressedContractFailures, { failure: "missing-slider-value" }),
    );
    expect(accessibleValueFailures(root)).toEqual(["slider has no accessible value"]);
  });
});
