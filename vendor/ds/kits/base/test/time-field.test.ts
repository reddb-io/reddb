import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import TimeEntryContractFailures from "./fixtures/TimeEntryContractFailures.svelte";
import TimeEntryConsumer from "./fixtures/TimeEntryConsumer.svelte";
import { TimeField, timeField } from "./fixtures/time-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const CLOCK_TIME = /^(?:[01]\d|2[0-3]):[0-5]\d$/;

function submittedTime(root: HTMLElement, name: string): string | null {
  return root.querySelector<HTMLInputElement>(`input[type="hidden"][name="${name}"]`)?.value ?? null;
}

describe("the deliberately failing TimeField fixture", () => {
  it("diagnoses an impossible time that reaches the form value", () => {
    const root = rendered(render(TimeEntryContractFailures, { failure: "impossible-time" }));

    expect(submittedTime(root, "meetingTime")).toBe("29:75");
    expect(submittedTime(root, "meetingTime")).not.toMatch(CLOCK_TIME);
  });
});

describe("the Base TimeField", () => {
  it("publishes complete clock times and refuses an impossible controlled value", () => {
    const valid = rendered(render(TimeField, {
      label: "Meeting time",
      name: "meetingTime",
      value: "09:45",
    }));
    const impossible = rendered(render(TimeField, {
      label: "Meeting time",
      name: "meetingTime",
      value: "29:75",
    }));

    expect(submittedTime(valid, "meetingTime")).toBe("09:45");
    expect(submittedTime(impossible, "meetingTime")).toBeNull();
    expect([...impossible.querySelectorAll<HTMLInputElement>("[data-time-segment]")].map(({ value }) => value))
      .toEqual(["", ""]);
  });

  it("accepts segmented keyboard entry without ever publishing an impossible time", () => {
    const root = rendered(render(TimeField, {
      label: "Meeting time",
      name: "meetingTime",
    }));
    const [hour, minute] = [...root.querySelectorAll<HTMLInputElement>("[data-time-segment]")];

    hour!.focus();
    hour!.value = "09";
    hour!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(document.activeElement).toBe(minute);
    expect(submittedTime(root, "meetingTime")).toBeNull();

    minute!.value = "45";
    minute!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(submittedTime(root, "meetingTime")).toBe("09:45");

    minute!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    expect(document.activeElement).toBe(hour);
    hour!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(minute);

    hour!.value = "29";
    hour!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(hour!.value).toBe("");
    expect(submittedTime(root, "meetingTime")).toBeNull();
  });

  it("carries the canonical Field associations and token appearance across both segments", () => {
    const root = rendered(render(TimeField, {
      label: "Meeting time",
      help: "Use local time.",
      error: "Choose an available time.",
      required: true,
      class: "max-w-xs",
      segmentClass: "tabular-nums",
    }));
    const [hour, minute] = [...root.querySelectorAll<HTMLInputElement>("[data-time-segment]")];
    const styles = timeField();

    expect(root.querySelector("label")?.htmlFor).toBe(hour!.id);
    expect(hour!.required).toBe(true);
    expect(minute!.required).toBe(true);
    expect(hour!.getAttribute("aria-describedby")).toBe(minute!.getAttribute("aria-describedby"));
    expect(hour!.getAttribute("aria-invalid")).toBe("true");
    expect(minute!.getAttribute("aria-invalid")).toBe("true");
    expect(classes(root)).toEqual(classesOf(styles.root({ class: "max-w-xs" })));
    expect(classes(hour!)).toEqual(classesOf(styles.segment({ class: "tabular-nums" })));
    expect(classes(root.querySelector("[data-time-segments]")!)).toEqual(classesOf(styles.segments()));
    expect(classes(hour!).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every appearance axis from a nested consumer scope", () => {
    const nested = render(TimeEntryConsumer)
      .querySelector<HTMLElement>("[data-nested-time-field] > div")!;

    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
