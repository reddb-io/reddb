import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import RangePressedConsumer from "./fixtures/RangePressedConsumer.svelte";
import { Rating, rating } from "./fixtures/range-pressed-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Rating", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Rating).toBeDefined();
    expect(rating).toBeTypeOf("function");
  });

  it("composes Fieldset and Label into one named, exclusive value", () => {
    const root = rendered(
      render(Rating, { legend: "Release confidence", name: "confidence", max: 5, value: 3 }),
    ) as HTMLFieldSetElement;
    const controls = [...root.querySelectorAll<HTMLInputElement>('input[type="radio"]')];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Release confidence");
    expect(controls).toHaveLength(5);
    expect(controls.map(({ name }) => name)).toEqual(Array(5).fill("confidence"));
    expect(controls.map(({ value }) => value)).toEqual(["1", "2", "3", "4", "5"]);
    expect(controls.filter(({ checked }) => checked).map(({ value }) => value)).toEqual(["3"]);
    expect(root.querySelector("output")?.textContent).toBe("3 of 5");
  });

  it("makes selection visible without colour and submits the selected native value", () => {
    const form = rendered(render(RangePressedConsumer, {}));
    const root = form.querySelector<HTMLElement>("[data-rating]")!;
    const controls = [...root.querySelectorAll<HTMLInputElement>('input[name="confidence"]')];

    expect([...root.querySelectorAll("[data-rating-mark]")].map(({ textContent }) => textContent)).toEqual([
      "★",
      "★",
      "★",
      "☆",
      "☆",
    ]);
    controls[4]!.click();
    flushSync();
    expect(controls.filter(({ checked }) => checked).map(({ value }) => value)).toEqual(["5"]);
    expect(root.querySelector("output")?.textContent).toBe("5 of 5");
    expect(new FormData(form as HTMLFormElement).get("confidence")).toBe("5");
  });

  it("takes help and an error through Fieldset, names them from every stop and marks in danger", () => {
    const root = rendered(
      render(Rating, {
        legend: "Support quality",
        name: "support",
        help: "Shared with the support team.",
        error: "Rate the support you received.",
        required: true,
      }),
    ) as HTMLFieldSetElement;
    const controls = [...root.querySelectorAll<HTMLInputElement>('input[type="radio"]')];
    const help = root.querySelector<HTMLElement>(":scope > p:not([role])")!;
    const error = root.querySelector<HTMLElement>(':scope > [role="alert"]')!;

    expect(error.textContent).toBe("Rate the support you received.");
    expect(classes(error).has("text-feedback-danger-foreground")).toBe(true);
    expect(controls).toHaveLength(5);
    for (const control of controls) {
      expect(control.getAttribute("aria-invalid")).toBe("true");
      expect(control.getAttribute("aria-errormessage")).toBe(error.id);
      expect(control.getAttribute("aria-describedby")).toBe(`${help.id} ${error.id}`);
    }
    for (const mark of root.querySelectorAll("[data-rating-mark]")) {
      expect(classes(mark).has("peer-aria-invalid:text-feedback-danger-foreground")).toBe(true);
    }
  });

  it("keeps native focus and keyboard events", () => {
    const onkeydown = vi.fn();
    const form = rendered(render(RangePressedConsumer, { ratingOnkeydown: onkeydown }));
    const control = form.querySelector<HTMLInputElement>('input[name="confidence"]')!;

    control.focus();
    expect(document.activeElement).toBe(control);
    control.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    flushSync();
    expect(onkeydown).toHaveBeenCalledTimes(1);
  });

  it("keeps its token appearance and axes free in a nested scope", () => {
    const form = rendered(render(RangePressedConsumer, {}));
    const root = form.querySelector<HTMLElement>("[data-appearance-scope] [data-rating]")!;
    const list = root.querySelector<HTMLElement>("[data-rating-list]")!;
    const styles = rating();

    expect(classes(list)).toEqual(classesOf(styles.list()));
    expect(classes(list).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
