import { describe, expect, it } from "vitest";
import TimeEntryContractFailures from "./fixtures/TimeEntryContractFailures.svelte";
import TimeEntryConsumer from "./fixtures/TimeEntryConsumer.svelte";
import { TimeRangeField, timeRangeField } from "./fixtures/time-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function formValue(root: HTMLElement, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

function isOrdered(start: string | null, end: string | null): boolean {
  return start !== null && end !== null && start <= end;
}

describe("the deliberately failing TimeRangeField fixture", () => {
  it("diagnoses a range that permits inverted bounds", () => {
    const root = rendered(render(TimeEntryContractFailures, { failure: "inverted-range" }));

    expect(isOrdered(
      root.querySelector<HTMLInputElement>('[name="supportStart"]')?.value ?? null,
      root.querySelector<HTMLInputElement>('[name="supportEnd"]')?.value ?? null,
    )).toBe(false);
  });
});

describe("the Base TimeRangeField", () => {
  it("publishes ordered bounds and refuses an inverted controlled range", () => {
    const ordered = rendered(render(TimeRangeField, {
      label: "Support window",
      startLabel: "Starts",
      endLabel: "Ends",
      startName: "supportStart",
      endName: "supportEnd",
      startValue: "09:00",
      endValue: "17:00",
    }));
    const inverted = rendered(render(TimeRangeField, {
      label: "Support window",
      startLabel: "Starts",
      endLabel: "Ends",
      startName: "supportStart",
      endName: "supportEnd",
      startValue: "17:00",
      endValue: "09:00",
    }));

    expect(isOrdered(formValue(ordered, "supportStart"), formValue(ordered, "supportEnd"))).toBe(true);
    expect(formValue(inverted, "supportStart")).toBe("17:00");
    expect(formValue(inverted, "supportEnd")).toBeNull();
    expect(inverted.getAttribute("aria-invalid")).toBe("true");
    expect(inverted.querySelector('[role="alert"]')?.textContent).toContain("before");
  });

  it("owns native group semantics while composing both canonical TimeFields", () => {
    const root = rendered(render(TimeRangeField, {
      label: "Support window",
      startLabel: "Starts",
      endLabel: "Ends",
      startValue: "17:00",
      endValue: "09:00",
      help: "Use local time.",
      rangeError: "The end must not be before the start.",
      required: true,
      class: "max-w-xl",
    }));
    const styles = timeRangeField();
    const segments = [...root.querySelectorAll<HTMLInputElement>("[data-time-segment]")];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Support window");
    expect(segments).toHaveLength(4);
    expect(segments.every(({ required }) => required)).toBe(true);
    expect(root.getAttribute("aria-describedby")).toContain("help");
    expect(root.getAttribute("aria-describedby")).toContain("error");
    expect(root.querySelector('[role="alert"]')?.textContent).toBe("The end must not be before the start.");
    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-xl" })));
    expect(classes(root.querySelector("[data-time-range-fields]")!)).toEqual(classesOf(styles.fields()));
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(TimeEntryConsumer)
      .querySelector<HTMLElement>("[data-nested-time-range-field] > fieldset")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
