import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import DateEntryContractFailures from "./fixtures/DateEntryContractFailures.svelte";
import DateEntryConsumer from "./fixtures/DateEntryConsumer.svelte";
import { DateField, dateField } from "./fixtures/date-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const ISO_DATE = /^\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])$/;

function submittedDate(root: HTMLElement, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

describe("the deliberately failing DateField fixture", () => {
  it("diagnoses an impossible date that reaches the form value", () => {
    const root = rendered(render(DateEntryContractFailures, { failure: "impossible-date" }));

    expect(submittedDate(root, "deploymentDate")).toBe("2026-02-30");
    expect(submittedDate(root, "deploymentDate")).toMatch(ISO_DATE);
  });
});

describe("the Base DateField", () => {
  it("publishes complete calendar dates and refuses an impossible controlled value", () => {
    const valid = rendered(render(DateField, {
      label: "Deployment date",
      name: "deploymentDate",
      value: "2026-08-20",
    }));
    const impossible = rendered(render(DateField, {
      label: "Deployment date",
      name: "deploymentDate",
      value: "2026-02-30",
    }));

    expect(submittedDate(valid, "deploymentDate")).toBe("2026-08-20");
    expect(submittedDate(impossible, "deploymentDate")).toBeNull();
    expect([...impossible.querySelectorAll<HTMLInputElement>("[data-date-segment]")].map(({ value }) => value))
      .toEqual(["", "", ""]);
  });

  it("accepts segmented keyboard entry without ever publishing an impossible date", () => {
    const root = rendered(render(DateField, {
      label: "Deployment date",
      name: "deploymentDate",
    }));
    const [year, month, day] = [...root.querySelectorAll<HTMLInputElement>("[data-date-segment]")];

    year!.focus();
    year!.value = "2026";
    year!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(document.activeElement).toBe(month);

    month!.value = "02";
    month!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(document.activeElement).toBe(day);

    day!.value = "30";
    day!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(submittedDate(root, "deploymentDate")).toBeNull();

    day!.value = "28";
    day!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(submittedDate(root, "deploymentDate")).toBe("2026-02-28");

    day!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    expect(document.activeElement).toBe(month);
    month!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    expect(document.activeElement).toBe(year);
    year!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(month);
    month!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(day);

    month!.value = "13";
    month!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(month!.value).toBe("");
    expect(submittedDate(root, "deploymentDate")).toBeNull();
  });

  it("carries the canonical Field associations and token appearance across every segment", () => {
    const root = rendered(render(DateField, {
      label: "Deployment date",
      help: "Use the release calendar.",
      error: "Choose an available date.",
      required: true,
      class: "max-w-sm",
      segmentClass: "tabular-nums",
    }));
    const [year, month, day] = [...root.querySelectorAll<HTMLInputElement>("[data-date-segment]")];
    const styles = dateField();

    expect(root.querySelector("label")?.htmlFor).toBe(year!.id);
    expect(year!.required).toBe(true);
    expect(month!.required).toBe(true);
    expect(day!.required).toBe(true);
    expect(year!.getAttribute("aria-describedby")).toBe(month!.getAttribute("aria-describedby"));
    expect(month!.getAttribute("aria-describedby")).toBe(day!.getAttribute("aria-describedby"));
    expect(year!.getAttribute("aria-invalid")).toBe("true");
    expect(month!.getAttribute("aria-invalid")).toBe("true");
    expect(day!.getAttribute("aria-invalid")).toBe("true");
    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-sm" })));
    expect(classes(year!)).toEqual(classesOf(styles.year({ class: "tabular-nums" })));
    expect(classes(month!)).toEqual(classesOf(styles.segment({ class: "tabular-nums" })));
    expect(classes(day!)).toEqual(classesOf(styles.segment({ class: "tabular-nums" })));
    expect(classes(root.querySelector("[data-date-segments]")!)).toEqual(classesOf(styles.segments()));
    expect(classes(month!).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(DateEntryConsumer)
      .querySelector<HTMLElement>("[data-nested-date-field] > div")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
