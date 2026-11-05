import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import DatePickerContractFailures from "./fixtures/DatePickerContractFailures.svelte";
import DatePickerConsumer from "./fixtures/DatePickerConsumer.svelte";
import { DateRangePicker, dateRangePicker } from "./fixtures/date-picker-consumer";
import { classes, classesOf, render, rendered } from "./mount";

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function formValue(root: ParentNode, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

function calendarBound(root: ParentNode, bound: "start" | "end"): string | null {
  return root.querySelector<HTMLElement>(`[data-bits-day][data-range-${bound}]`)?.dataset.value ?? null;
}

function synchronizationFailures(root: ParentNode): string[] {
  return formValue(root, "deploymentStart") === calendarBound(root, "start")
    && formValue(root, "deploymentEnd") === calendarBound(root, "end")
    ? []
    : ["date range fields and range calendar are desynchronized"];
}

describe("the deliberately failing DateRangePicker fixture", () => {
  it("diagnoses field bounds and calendar endpoints with different dates", () => {
    const root = rendered(render(DatePickerContractFailures, {
      failure: "date-range-picker-desynchronization",
    }));

    expect(synchronizationFailures(root)).toEqual([
      "date range fields and range calendar are desynchronized",
    ]);
  });
});

describe("the Base DateRangePicker", () => {
  it("keeps partial selection open and publishes a synchronized complete range", async () => {
    const onvaluechange = vi.fn();
    const root = render(DateRangePicker, {
      label: "Deployment window",
      startLabel: "Starts",
      endLabel: "Ends",
      calendarLabel: "Deployment window calendar",
      startName: "deploymentStart",
      endName: "deploymentEnd",
      startValue: "2026-06-10",
      endValue: "2026-06-12",
      open: true,
      onvaluechange,
    });
    await settle();

    expect(synchronizationFailures(document)).toEqual([]);
    document.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-15"]')!.click();
    await settle();
    expect(document.querySelector("[data-popover-surface]")).not.toBeNull();
    expect(formValue(root, "deploymentStart")).toBe("2026-06-15");
    expect(formValue(root, "deploymentEnd")).toBeNull();

    document.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-18"]')!.click();
    await settle();
    expect(formValue(root, "deploymentEnd")).toBe("2026-06-18");
    expect(onvaluechange).toHaveBeenLastCalledWith({
      start: "2026-06-15",
      end: "2026-06-18",
    });
    expect(document.querySelector("[data-popover-surface]")).toBeNull();
  });

  it("composes canonical appearance without selecting an axis", () => {
    const root = rendered(render(DateRangePicker, {
      label: "Deployment window",
      startLabel: "Starts",
      endLabel: "Ends",
      calendarLabel: "Deployment window calendar",
      class: "max-w-2xl",
    }));
    const styles = dateRangePicker();

    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-2xl" })));
    expect(root.querySelector("[data-date-range-field]")).not.toBeNull();
    expect(root.querySelector("[data-popover-trigger]")).not.toBeNull();
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const root = render(DatePickerConsumer)
      .querySelector<HTMLElement>("[data-nested-date-range-picker] > [data-date-range-picker]")!;

    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
    expect(classes(root.querySelector("[data-date-segment]")!).has(
      "h-[var(--reddb-spatial-control-height-md)]",
    )).toBe(true);
  });
});
