import { CalendarDate } from "@internationalized/date";
import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import CalendarContractFailures from "./fixtures/CalendarContractFailures.svelte";
import CalendarConsumer from "./fixtures/CalendarConsumer.svelte";
import { calendar, RangeCalendar } from "./fixtures/calendar-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function selectionAnnouncementFailures(root: HTMLElement): string[] {
  const politeLog = root.ownerDocument.querySelector<HTMLElement>(
    '[data-bits-announcer] [aria-live="polite"]',
  );
  const announcements = politeLog
    ? [...politeLog.children].map((message) => (message as HTMLElement).innerText)
    : [];
  return announcements.some((message) => message?.startsWith("Selected Dates:"))
    ? []
    : ["selected range is not politely announced"];
}

describe("the deliberately failing RangeCalendar fixture", () => {
  it("diagnoses a selected range without an announcement", () => {
    const root = rendered(render(CalendarContractFailures, {
      failure: "range-without-announcement",
    }));

    expect(selectionAnnouncementFailures(root)).toEqual([
      "selected range is not politely announced",
    ]);
  });
});

describe("the Base RangeCalendar", () => {
  it("renders no <header>, so a RangeCalendar in a popover adds no banner landmark (wave 5A)", () => {
    const root = rendered(render(RangeCalendar, {
      label: "Release window",
      placeholder: new CalendarDate(2026, 6, 10),
    }));
    expect(root.querySelector("header")).toBeNull();
    expect(root.querySelector("[data-range-calendar-header]")?.tagName).toBe("DIV");
  });

  it("is available with the Calendar appearance seam through the Base consumer subpath", () => {
    expect(RangeCalendar).toBeDefined();
    expect(calendar).toBeTypeOf("function");
  });

  it("shares the roving keyboard grid and keeps focus on one date", async () => {
    const root = rendered(render(RangeCalendar, {
      label: "Deployment window",
      placeholder: new CalendarDate(2026, 6, 10),
    }));
    const focused = root.querySelector<HTMLElement>(
      '[data-bits-day][data-value="2026-06-10"]',
    )!;

    focused.focus();
    focused.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await tick();
    flushSync();

    expect(document.activeElement?.getAttribute("data-value")).toBe("2026-06-11");
    expect(root.querySelectorAll('[data-bits-day][tabindex="0"]')).toHaveLength(1);
  });

  it("selects and politely announces an inclusive range without relying on colour", () => {
    const onvaluechange = vi.fn();
    const root = rendered(render(RangeCalendar, {
      label: "Deployment window",
      placeholder: new CalendarDate(2026, 6, 10),
      onvaluechange,
    }));

    root.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-10"]')!.click();
    root.querySelector<HTMLElement>('[data-bits-day][data-value="2026-06-12"]')!.click();
    flushSync();

    const selected = onvaluechange.mock.calls.at(-1)?.[0];
    expect(selected?.start?.toString()).toBe("2026-06-10");
    expect(selected?.end?.toString()).toBe("2026-06-12");
    expect(root.querySelector('[data-bits-day][data-range-start]')?.textContent).toContain("10");
    expect(root.querySelector('[data-bits-day][data-range-middle]')?.textContent).toContain("11");
    expect(root.querySelector('[data-bits-day][data-range-end]')?.textContent).toContain("12");
    expect(root.querySelector('[data-range-start][role="gridcell"]')?.getAttribute("aria-selected"))
      .toBe("true");
    expect(selectionAnnouncementFailures(root)).toEqual([]);
  });

  it("shares Density-owned geometry and inherits every nested appearance axis", () => {
    const consumer = render(CalendarConsumer);
    const root = consumer.querySelector<HTMLElement>("[data-nested-range-calendar]")!;
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
