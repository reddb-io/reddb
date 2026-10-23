import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import ComboboxConsumer from "./fixtures/ComboboxConsumer.svelte";
import ComboboxContractFailures from "./fixtures/ComboboxContractFailures.svelte";
import {
  Combobox,
  combobox,
  input as inputAppearance,
  popover as popoverAppearance,
} from "./fixtures/combobox-consumer";
import { classes, classesOf, render } from "./mount";

const regions = [
  { value: "us-east-1", label: "North America" },
  { value: "sa-east-1", label: "South America" },
];

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function activeDescendantFailures(root: HTMLElement): string[] {
  const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;
  input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
  flushSync();
  const activeId = input.getAttribute("aria-activedescendant");
  return activeId && document.getElementById(activeId)?.getAttribute("role") === "option"
    ? []
    : ["keyboard highlight is not exposed through aria-activedescendant"];
}

function retainedInputFailures(root: HTMLElement): string[] {
  const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;
  input.value = "south";
  input.dispatchEvent(new InputEvent("input", { bubbles: true }));
  flushSync();
  return input.value === "south" ? [] : ["typed input was discarded"];
}

describe("the Base Combobox", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Combobox).toBeDefined();
    expect(combobox).toBeTypeOf("function");
  });

  it("composes Field into one visibly named typeahead control", () => {
    const root = render(Combobox, {
      label: "Region",
      help: "Type to narrow the available regions.",
      name: "region",
      options: regions,
      required: true,
    });
    const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;
    const label = root.querySelector<HTMLLabelElement>(`label[for="${input.id}"]`);

    expect(label?.textContent).toContain("Region");
    expect(input.required).toBe(true);
    expect(input.getAttribute("aria-describedby")).not.toBeNull();
    expect(input.getAttribute("aria-autocomplete")).toBe("list");
  });

  it("retains typed input while narrowing the caller-owned options", async () => {
    const root = render(Combobox, { label: "Region", options: regions });
    const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "s", bubbles: true }));
    input.value = "south";
    input.dispatchEvent(new InputEvent("input", { bubbles: true }));
    await settle();

    expect(input.value).toBe("south");
    expect(
      [...document.querySelectorAll('[role="option"]')].map((option) => option.textContent?.trim()),
    ).toEqual(["South America"]);
  });

  it("keeps focus on the input while active-descendant navigation selects an option", async () => {
    const root = render(Combobox, { label: "Region", options: regions });
    const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;
    input.focus();

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    const activeId = input.getAttribute("aria-activedescendant");
    const activeOption = activeId ? document.getElementById(activeId) : null;

    expect(document.activeElement).toBe(input);
    expect(activeOption?.getAttribute("role")).toBe("option");
    expect(activeOption?.textContent).toContain("North America");

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();

    expect(input.value).toBe("South America");
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(input);
  });

  it("submits one selected value and announces selection without relying on color", async () => {
    const onvaluechange = vi.fn();
    const root = render(ComboboxConsumer, { required: true, onvaluechange });
    const form = root.querySelector<HTMLFormElement>("form")!;
    const input = form.querySelector<HTMLInputElement>('[role="combobox"]')!;

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();

    expect(new FormData(form).get("region")).toBe("sa-east-1");
    expect(onvaluechange).toHaveBeenCalledWith("sa-east-1");
    expect(input.value).toBe("South America");

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    const option = document.querySelector<HTMLElement>('[role="option"][data-value="sa-east-1"]')!;
    expect(option.getAttribute("aria-selected")).toBe("true");
    expect(option.textContent).toContain("Selected");

    // Highlight and selection are neutral: the Brand red never fills an option.
    const names = classes(option);
    expect(names.has("data-[highlighted]:bg-foreground/10")).toBe(true);
    expect(names.has("data-[selected]:font-medium")).toBe(true);
    expect([...names].some((name) => /(?:^|:)bg-primary(?:\/|$)|text-on-primary/.test(name))).toBe(false);
  });

  it("renders an externally selected value as its visible option label", () => {
    const root = render(ComboboxConsumer, { value: "sa-east-1" });
    const form = root.querySelector<HTMLFormElement>("form")!;
    const input = root.querySelector<HTMLInputElement>('[role="combobox"]')!;

    expect(input.value).toBe("South America");
    expect(new FormData(form).get("region")).toBe("sa-east-1");
  });

  it("composes Input and Popover appearance inside a nested axis scope", async () => {
    const root = render(ComboboxConsumer, {});
    const scope = root.querySelector<HTMLElement>("[data-appearance-scope]")!;
    const input = scope.querySelector<HTMLInputElement>('[role="combobox"]')!;
    const styles = combobox();

    expect(classes(input)).toEqual(classesOf(inputAppearance({ class: styles.input() })));
    expect(classes(input).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(scope.querySelector("[data-combobox]")?.parentElement?.className).toContain("max-w-md");

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    const surface = document.querySelector<HTMLElement>("[data-combobox-surface]")!;
    expect(classes(surface)).toEqual(classesOf(popoverAppearance({ class: styles.content() })));
    expect(classes(surface).has("motion-reduce:transition-none")).toBe(true);
    expect(surface.dataset.avoidCollisions).toBe("true");
  });
});

describe("the deliberately failing Combobox fixtures", () => {
  it("diagnoses missing active-descendant semantics", () => {
    const root = render(ComboboxContractFailures, { failure: "missing-active-descendant" });
    expect(activeDescendantFailures(root)).toEqual([
      "keyboard highlight is not exposed through aria-activedescendant",
    ]);
  });

  it("diagnoses a typeahead that discards typed input", () => {
    const root = render(ComboboxContractFailures, { failure: "discarded-input" });
    expect(retainedInputFailures(root)).toEqual(["typed input was discarded"]);
  });
});
