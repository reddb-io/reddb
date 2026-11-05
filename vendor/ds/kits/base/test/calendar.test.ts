import { CalendarDate } from "@internationalized/date";
import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import CalendarContractFailures from "./fixtures/CalendarContractFailures.svelte";
import CalendarConsumer from "./fixtures/CalendarConsumer.svelte";
import { Calendar, calendar } from "./fixtures/calendar-consumer";
import { classes, classesOf, render, rendered } from "./mount";

async function keyboardNavigationFailures(root: HTMLElement): Promise<string[]> {
  const active = root.querySelector<HTMLElement>('[data-bits-day][tabindex="0"]');
  if (!active) return ["calendar has no date in the tab order"];

  active.focus();
  active.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  await tick();
  flushSync();
  return document.activeElement !== active ? [] : ["arrow keys do not move date focus"];
}

describe("the deliberately failing Calendar fixture", () => {
  it("diagnoses a calendar grid without keyboard navigation", async () => {
    const root = rendered(render(CalendarContractFailures, {
      failure: "calendar-without-keyboard-navigation",
    }));

    await expect(keyboardNavigationFailures(root)).resolves.toEqual([
      "calendar has no date in the tab order",
    ]);
  });
});

describe("the Base Calendar", () => {
  it("is available with its shared appearance seam through the Base consumer subpath", () => {
    expect(Calendar).toBeDefined();
    expect(calendar).toBeTypeOf("function");
  });

  it("renders no <header>, so a Calendar in a popover adds no banner landmark (wave 5A)", () => {
    const root = rendered(render(Calendar, {
      label: "Deployment date",
      placeholder: new CalendarDate(2026, 6, 10),
    }));
    expect(root.querySelector("header")).toBeNull();
    expect(root.querySelector("[data-calendar-header]")?.tagName).toBe("DIV");
  });

  it("provides a roving date tab stop and arrow-key focus navigation", async () => {
    const root = rendered(render(Calendar, {
      label: "Deployment date",
      placeholder: new CalendarDate(2026, 6, 10),
    }));
    await expect(keyboardNavigationFailures(root)).resolves.toEqual([]);
    expect(document.activeElement?.getAttribute("data-value")).toBe("2026-06-11");
  });

  it("selects one date and exposes selection semantically as well as visually", () => {
    const onvaluechange = vi.fn();
    const root = rendered(render(Calendar, {
      label: "Deployment date",
      placeholder: new CalendarDate(2026, 6, 10),
      onvaluechange,
    }));
    const day = root.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-12"]')!;

    day.click();
    flushSync();

    expect(onvaluechange.mock.calls.at(-1)?.[0]?.toString()).toBe("2026-06-12");
    expect(day.hasAttribute("data-selected")).toBe(true);
    expect(day.closest('[role="gridcell"]')?.getAttribute("aria-selected")).toBe("true");
  });

  it("keeps Density-owned geometry and inherits every nested appearance axis", () => {
    const consumer = render(CalendarConsumer);
    const root = consumer.querySelector<HTMLElement>("[data-nested-calendar]")!;
    const styles = calendar();

    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-sm" })));
    expect(classes(root.querySelector("[data-bits-day]")!).has(
      "size-[var(--reddb-spatial-control-height-sm)]",
    )).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
