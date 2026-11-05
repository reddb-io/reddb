// One event dialect (ADR 0026), proven through the public Base subpath.
//
// A value control is `bind:value` plus `onvaluechange(value)`; a disclosure
// or overlay is `bind:open` plus `onopenchange(open)`; any other bindable
// state `<state>` reports through `on<state>change`. A native event name is
// never reused with a payload that is not that Event, so `onchange` keeps the
// native Event on Checkbox, Switch and Select, and the renamed DS callbacks
// stay one release as aliases that still fire and warn once in development.
// `onselect` (item activation) is the sanctioned exception and is untouched.

import { createRawSnippet, flushSync } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import {
  Accordion,
  Carousel,
  Checkbox,
  DateField,
  Disclosure,
  Input,
  RadioGroup,
  Select,
  Slider,
  Swap,
  Switch,
  Tabs,
  Textarea,
  ToggleButton,
} from "@reddb-io/design-system/base";
import { render, rendered } from "./mount";

const TAB_ITEMS = [
  { value: "overview", label: "Overview" },
  { value: "events", label: "Events" },
];
const SECTIONS = [
  { value: "changes", label: "What changed?" },
  { value: "impact", label: "What is the impact?" },
];
const slides = ["One", "Two"].map((label) => ({
  label,
  content: createRawSnippet(() => ({ render: () => `<p>${label}</p>` })),
}));

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  flushSync();
}

function type(element: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string, event = "input"): void {
  element.value = value;
  element.dispatchEvent(new Event(event, { bubbles: true }));
  flushSync();
}

let warn: MockInstance<typeof console.warn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  warn.mockRestore();
});

const warnings = () => warn.mock.calls.map(([message]) => String(message));

describe("the canonical callbacks", () => {
  it("reports a value change through onvaluechange on Tabs, Accordion and Carousel", () => {
    const tabsChange = vi.fn();
    const tabs = rendered(render(Tabs, { label: "View", items: TAB_ITEMS, onvaluechange: tabsChange }));
    const [first] = [...tabs.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
    first!.focus();
    press(first!, "ArrowRight");
    expect(tabsChange).toHaveBeenCalledWith("events");

    const accordionChange = vi.fn();
    const accordion = rendered(render(Accordion, { label: "Questions", items: SECTIONS, onvaluechange: accordionChange }));
    accordion.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")[1]!.click();
    flushSync();
    expect(accordionChange).toHaveBeenLastCalledWith(["impact"]);

    const carouselChange = vi.fn();
    const carousel = rendered(render(Carousel, { label: "Slides", slides, onvaluechange: carouselChange }));
    carousel.querySelector<HTMLButtonElement>("[aria-label='Next slide']")!.click();
    flushSync();
    expect(carouselChange).toHaveBeenCalledWith(1);
    expect(warnings()).toEqual([]);
  });

  it("reports open state through onopenchange and other bindable state through on<state>change", () => {
    const openChange = vi.fn();
    const disclosure = rendered(render(Disclosure, { label: "Details", onopenchange: openChange }));
    disclosure.querySelector<HTMLButtonElement>("button")!.click();
    flushSync();
    expect(openChange).toHaveBeenCalledWith(true);

    const swappedChange = vi.fn();
    rendered(render(Swap, { label: "Show detail", onswappedchange: swappedChange })).click();
    flushSync();
    expect(swappedChange).toHaveBeenCalledWith(true);

    const pressedChange = vi.fn();
    rendered(render(ToggleButton, { label: "Bold", onpressedchange: pressedChange })).click();
    flushSync();
    expect(pressedChange).toHaveBeenCalledWith(true);
    expect(warnings()).toEqual([]);
  });

  it("gives every value control onvaluechange while native events keep their Event", () => {
    const inputChange = vi.fn();
    const nativeInput = vi.fn();
    const input = rendered(render(Input, { "aria-label": "Name", onvaluechange: inputChange, oninput: nativeInput })) as HTMLInputElement;
    type(input, "Ada");
    expect(inputChange).toHaveBeenCalledWith("Ada");
    expect(nativeInput.mock.calls[0]![0]).toBeInstanceOf(Event);

    const textareaChange = vi.fn();
    const textarea = rendered(render(Textarea, { "aria-label": "Notes", onvaluechange: textareaChange })) as HTMLTextAreaElement;
    type(textarea, "Hello");
    expect(textareaChange).toHaveBeenCalledWith("Hello");

    const selectChange = vi.fn();
    const nativeChange = vi.fn();
    const select = rendered(
      render(Select, {
        "aria-label": "Region",
        options: [
          { value: "eu", label: "Europe" },
          { value: "us", label: "United States" },
        ],
        onvaluechange: selectChange,
        onchange: nativeChange,
      }),
    ) as HTMLSelectElement;
    type(select, "us", "change");
    expect(selectChange).toHaveBeenCalledWith("us");
    expect(nativeChange.mock.calls[0]![0]).toBeInstanceOf(Event);

    const sliderChange = vi.fn();
    const slider = rendered(render(Slider, { label: "Volume", onvaluechange: sliderChange }))
      .querySelector<HTMLInputElement>('input[type="range"]')!;
    type(slider, "40");
    expect(sliderChange).toHaveBeenCalledWith(40);

    const radioChange = vi.fn();
    const radio = rendered(
      render(RadioGroup, {
        legend: "Channel",
        name: "channel",
        options: [
          { value: "stable", label: "Stable" },
          { value: "beta", label: "Beta" },
        ],
        onvaluechange: radioChange,
      }),
    );
    radio.querySelectorAll<HTMLInputElement>('input[type="radio"]')[1]!.click();
    flushSync();
    expect(radioChange).toHaveBeenCalledWith("beta");

    const dateChange = vi.fn();
    const date = rendered(render(DateField, { label: "Start", onvaluechange: dateChange }));
    const [year, month, day] = [...date.querySelectorAll<HTMLInputElement>("input")];
    type(year!, "2026");
    type(month!, "10");
    expect(dateChange).not.toHaveBeenCalled();
    type(day!, "05");
    expect(dateChange).toHaveBeenCalledWith("2026-10-05");
    expect(warnings()).toEqual([]);
  });

  it("reports checked state through oncheckedchange beside the native onchange Event", () => {
    for (const Control of [Checkbox, Switch]) {
      const checkedChange = vi.fn();
      const nativeChange = vi.fn();
      const input = rendered(render(Control, { label: "Notify me", oncheckedchange: checkedChange, onchange: nativeChange }))
        .querySelector<HTMLInputElement>("input")!;
      input.click();
      flushSync();
      expect(checkedChange).toHaveBeenCalledWith(true);
      expect(nativeChange.mock.calls[0]![0]).toBeInstanceOf(Event);
    }
  });
});

describe("the deprecated aliases (one release)", () => {
  it("still fire, and warn once per name in development", () => {
    const tabsChange = vi.fn();
    for (const _ of [1, 2]) {
      const tabs = rendered(render(Tabs, { label: "View", items: TAB_ITEMS, onchange: tabsChange }));
      const [first] = [...tabs.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
      first!.focus();
      press(first!, "ArrowRight");
    }
    expect(tabsChange).toHaveBeenCalledWith("events");

    const disclosureChange = vi.fn();
    rendered(render(Disclosure, { label: "Details", onchange: disclosureChange }))
      .querySelector<HTMLButtonElement>("button")!
      .click();
    flushSync();
    expect(disclosureChange).toHaveBeenCalledWith(true);

    const carouselChange = vi.fn();
    rendered(render(Carousel, { label: "Slides", slides, onchange: carouselChange }))
      .querySelector<HTMLButtonElement>("[aria-label='Next slide']")!
      .click();
    flushSync();
    expect(carouselChange).toHaveBeenCalledWith(1);

    const swapChange = vi.fn();
    rendered(render(Swap, { label: "Show detail", onchange: swapChange })).click();
    flushSync();
    expect(swapChange).toHaveBeenCalledWith(true);

    expect(warnings()).toEqual([
      expect.stringMatching(/\[reddb Tabs\] onchange is deprecated.*onvaluechange/),
      expect.stringMatching(/\[reddb Disclosure\] onchange is deprecated.*onopenchange/),
      expect.stringMatching(/\[reddb Carousel\] onchange is deprecated.*onvaluechange/),
      expect.stringMatching(/\[reddb Swap\] onchange is deprecated.*onswappedchange/),
    ]);
  });

  it("keeps Accordion's bound expanded working beside the canonical value", () => {
    const accordionChange = vi.fn();
    const root = rendered(
      render(Accordion, { label: "Questions", items: SECTIONS, expanded: ["changes"], onchange: accordionChange }),
    );
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")];
    expect(controls.map(({ ariaExpanded }) => ariaExpanded)).toEqual(["true", "false"]);
    controls[1]!.click();
    flushSync();
    expect(controls.map(({ ariaExpanded }) => ariaExpanded)).toEqual(["false", "true"]);
    expect(accordionChange).toHaveBeenLastCalledWith(["impact"]);
    expect(warnings()).toEqual([
      expect.stringMatching(/\[reddb Accordion\] expanded is deprecated.*bind:value/),
      expect.stringMatching(/\[reddb Accordion\] onchange is deprecated.*onvaluechange/),
    ]);
  });
});

