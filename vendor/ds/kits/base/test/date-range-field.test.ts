import { describe, expect, it } from "vitest";
import DateEntryContractFailures from "./fixtures/DateEntryContractFailures.svelte";
import DateEntryConsumer from "./fixtures/DateEntryConsumer.svelte";
import { DateRangeField, dateRangeField } from "./fixtures/date-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function formValue(root: HTMLElement, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

function isOrdered(start: string | null, end: string | null): boolean {
  return start !== null && end !== null && start <= end;
}

describe("the deliberately failing DateRangeField fixture", () => {
  it("diagnoses a range that permits inverted bounds", () => {
    const root = rendered(render(DateEntryContractFailures, { failure: "inverted-range" }));

    expect(isOrdered(
      root.querySelector<HTMLInputElement>('[name="deploymentStart"]')?.value ?? null,
      root.querySelector<HTMLInputElement>('[name="deploymentEnd"]')?.value ?? null,
    )).toBe(false);
  });
});

describe("the Base DateRangeField", () => {
  it("publishes ordered bounds and refuses an inverted controlled range", () => {
    const ordered = rendered(render(DateRangeField, {
      label: "Deployment window",
      startLabel: "Starts",
      endLabel: "Ends",
      startName: "deploymentStart",
      endName: "deploymentEnd",
      startValue: "2026-08-10",
      endValue: "2026-08-20",
    }));
    const inverted = rendered(render(DateRangeField, {
      label: "Deployment window",
      startLabel: "Starts",
      endLabel: "Ends",
      startName: "deploymentStart",
      endName: "deploymentEnd",
      startValue: "2026-08-20",
      endValue: "2026-08-10",
    }));

    expect(isOrdered(formValue(ordered, "deploymentStart"), formValue(ordered, "deploymentEnd"))).toBe(true);
    expect(formValue(inverted, "deploymentStart")).toBe("2026-08-20");
    expect(formValue(inverted, "deploymentEnd")).toBeNull();
    expect(inverted.getAttribute("aria-invalid")).toBe("true");
    expect(inverted.querySelector('[role="alert"]')?.textContent).toContain("before");
  });

  it("owns native group semantics while composing both canonical DateFields", () => {
    const root = rendered(render(DateRangeField, {
      label: "Deployment window",
      startLabel: "Starts",
      endLabel: "Ends",
      startValue: "2026-08-20",
      endValue: "2026-08-10",
      help: "Use the release calendar.",
      rangeError: "The end must not be before the start.",
      required: true,
      class: "max-w-2xl",
    }));
    const styles = dateRangeField();
    const segments = [...root.querySelectorAll<HTMLInputElement>("[data-date-segment]")];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Deployment window");
    expect(segments).toHaveLength(6);
    expect(segments.every(({ required }) => required)).toBe(true);
    expect(segments.every(({ tabIndex }) => tabIndex === 0)).toBe(true);
    expect(root.getAttribute("aria-describedby")).toContain("help");
    expect(root.getAttribute("aria-describedby")).toContain("error");
    expect(root.querySelector('[role="alert"]')?.textContent).toBe("The end must not be before the start.");
    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-2xl" })));
    expect(classes(root.querySelector("[data-date-range-fields]")!)).toEqual(classesOf(styles.fields()));
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);

    segments[3]!.focus();
    segments[3]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(segments[4]);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(DateEntryConsumer)
      .querySelector<HTMLElement>("[data-nested-date-range-field] > fieldset")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
