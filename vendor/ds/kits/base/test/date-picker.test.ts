import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import DatePickerContractFailures from "./fixtures/DatePickerContractFailures.svelte";
import DatePickerConsumer from "./fixtures/DatePickerConsumer.svelte";
import { DatePicker, datePicker } from "./fixtures/date-picker-consumer";
import { classes, classesOf, render, rendered } from "./mount";

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function selectedDate(root: ParentNode): string | null {
  return root.querySelector<HTMLElement>("[data-bits-day][data-selected]")?.dataset.value ?? null;
}

function submittedDate(root: ParentNode, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

function synchronizationFailures(root: ParentNode, name: string): string[] {
  return submittedDate(root, name) === selectedDate(root)
    ? []
    : ["date field and calendar selection are desynchronized"];
}

describe("the deliberately failing DatePicker fixture", () => {
  it("diagnoses field and calendar state with different dates", () => {
    const root = rendered(render(DatePickerContractFailures, {
      failure: "date-picker-desynchronization",
    }));

    expect(synchronizationFailures(root, "deploymentDate")).toEqual([
      "date field and calendar selection are desynchronized",
    ]);
  });
});

describe("the Base DatePicker", () => {
  it("publishes one synchronized date through the distributed Base seam", async () => {
    const onvaluechange = vi.fn();
    const root = render(DatePicker, {
      label: "Deployment date",
      calendarLabel: "Deployment date calendar",
      name: "deploymentDate",
      value: "2026-06-10",
      open: true,
      onvaluechange,
    });
    await settle();

    expect(synchronizationFailures(document, "deploymentDate")).toEqual([]);
    document.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-12"]')!.click();
    await settle();

    expect(submittedDate(root, "deploymentDate")).toBe("2026-06-12");
    expect(onvaluechange).toHaveBeenLastCalledWith("2026-06-12");
    expect(document.querySelector("[data-popover-surface]")).toBeNull();
  });

  it("composes canonical appearance without selecting an axis", () => {
    const root = rendered(render(DatePicker, {
      label: "Deployment date",
      class: "max-w-sm",
    }));
    const styles = datePicker();

    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-sm" })));
    expect(root.querySelector("[data-date-field]")).not.toBeNull();
    expect(root.querySelector("[data-popover-trigger]")).not.toBeNull();
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("specializes the Popover to the Calendar's intrinsic width", async () => {
    render(DatePicker, {
      label: "Deployment date",
      calendarLabel: "Deployment date calendar",
      open: true,
      contentClass: "data-[consumer]:block",
    });
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-popover-surface]")!;
    for (const token of classesOf(
      datePicker().content({ class: "data-[consumer]:block" }),
    )) {
      expect(surface.classList.contains(token), token).toBe(true);
    }
  });

  it("keeps focus inside the open calendar and returns it to the trigger on Escape", async () => {
    const root = render(DatePicker, {
      label: "Deployment date",
      calendarLabel: "Deployment date calendar",
      value: "2026-06-10",
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;
    trigger.focus();
    trigger.click();
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-popover-surface]")!;
    expect(surface.contains(document.activeElement)).toBe(true);
    document.activeElement?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();

    expect(document.querySelector("[data-popover-surface]")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const root = render(DatePickerConsumer)
      .querySelector<HTMLElement>("[data-nested-date-picker] > [data-date-picker]")!;

    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
