import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import RangePressedConsumer from "./fixtures/RangePressedConsumer.svelte";
import RangePressedContractFailures from "./fixtures/RangePressedContractFailures.svelte";
import { ToggleGroup, toggleGroup } from "./fixtures/range-pressed-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const OPTIONS = [
  { value: "left", label: "Left" },
  { value: "center", label: "Center" },
  { value: "right", label: "Right" },
] as const;

function singleSelectionFailures(root: HTMLElement): string[] {
  const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];
  return buttons.length > 0 && buttons.filter((button) => button.ariaPressed === "true").length === 1
    ? []
    : ["toggle group does not expose exactly one pressed option"];
}

describe("the Base ToggleGroup", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(ToggleGroup).toBeDefined();
    expect(toggleGroup).toBeTypeOf("function");
  });

  it("composes Fieldset and ToggleButton into one named single selection", () => {
    const root = rendered(
      render(ToggleGroup, {
        legend: "Alignment",
        options: OPTIONS,
        value: "center",
      }),
    );
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Alignment");
    expect(root.querySelector("[role='group']")).not.toBeNull();
    expect(buttons.map(({ textContent }) => textContent?.trim())).toEqual(["Left", "Center", "Right"]);
    expect(buttons.map(({ ariaPressed }) => ariaPressed)).toEqual(["false", "true", "false"]);
    expect(buttons.map(({ tabIndex }) => tabIndex)).toEqual([-1, 0, -1]);
    expect(singleSelectionFailures(root)).toEqual([]);
  });

  it("moves the single selection by click and submits it", () => {
    const onvaluechange = vi.fn();
    const form = rendered(render(RangePressedConsumer, { groupOnvaluechange: onvaluechange }));
    const root = form.querySelector<HTMLElement>("[data-toggle-group]")!;
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];

    buttons[2]!.click();
    flushSync();
    expect(buttons.map(({ ariaPressed }) => ariaPressed)).toEqual(["false", "false", "true"]);
    expect(singleSelectionFailures(root)).toEqual([]);
    expect(new FormData(form as HTMLFormElement).get("alignment")).toBe("right");
    expect(onvaluechange).toHaveBeenCalledWith("right");

    buttons[2]!.click();
    flushSync();
    expect(singleSelectionFailures(root)).toEqual([]);
  });

  it("uses roving focus and arrow keys to select one enabled option", () => {
    const root = rendered(
      render(ToggleGroup, { legend: "Alignment", options: OPTIONS, value: "left" }),
    );
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];

    buttons[0]!.focus();
    buttons[0]!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }),
    );
    flushSync();
    expect(document.activeElement).toBe(buttons[1]);
    expect(buttons.map(({ ariaPressed }) => ariaPressed)).toEqual(["false", "true", "false"]);
    expect(buttons.map(({ tabIndex }) => tabIndex)).toEqual([-1, 0, -1]);
  });

  it("takes help and an error through Fieldset, names them from every option and edges the group in danger", () => {
    const root = rendered(
      render(ToggleGroup, {
        legend: "Alignment",
        options: OPTIONS,
        value: "center",
        help: "Applies to every paragraph.",
        error: "Justified text is not available here.",
      }),
    );
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];
    const list = root.querySelector<HTMLElement>("[role='group']")!;
    const help = root.querySelector<HTMLElement>(":scope > p:not([role])")!;
    const error = root.querySelector<HTMLElement>(':scope > [role="alert"]')!;

    expect(error.textContent).toBe("Justified text is not available here.");
    expect(classes(error).has("text-feedback-danger-foreground")).toBe(true);
    for (const button of buttons) {
      expect(button.getAttribute("aria-invalid")).toBe("true");
      expect(button.getAttribute("aria-errormessage")).toBe(error.id);
      expect(button.getAttribute("aria-describedby")).toBe(`${help.id} ${error.id}`);
    }
    expect(classes(list)).toEqual(classesOf(toggleGroup({ invalid: true }).list()));
    expect(classes(list).has("border-feedback-danger-border")).toBe(true);
    expect(classes(list).has("border-control-edge")).toBe(false);
    // The separators between options keep the control edge.
    for (const button of buttons) expect(classes(button).has("border-e-control-edge")).toBe(true);
    expect(buttons.map(({ ariaPressed }) => ariaPressed)).toEqual(["false", "true", "false"]);
  });

  // Maintainer decision (2026-10-06): the standard segmented control. The
  // group is exactly one Button tall (control-height-md, border included) and
  // its segments sit flush, split by control-edge dividers; this replaces the
  // inset the group carried since issue 423.
  it("draws a one-Button-tall border-role contour with flush, divided segments", () => {
    const form = rendered(render(RangePressedConsumer, {}));
    const root = form.querySelector<HTMLElement>("[data-appearance-scope] [data-toggle-group]")!;
    const list = root.querySelector<HTMLElement>("[role='group']")!;
    const options = [...list.querySelectorAll<HTMLElement>("button")];
    const styles = toggleGroup();

    expect(classes(list)).toEqual(classesOf(styles.list()));
    expect(options).toHaveLength(3);
    expect(options[0]!.nextElementSibling).toBe(options[1]);
    expect(classes(list).has("gap-0")).toBe(true);
    expect(classes(list).has("border-control-edge")).toBe(true);
    expect(classes(list).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(classes(list).has("box-border")).toBe(true);
    expect([...classes(list)].some((name) => /^p[xy]?-/.test(name))).toBe(false);
    expect(classes(list).has("border-muted")).toBe(false);
    expect(classes(list).has("rounded-md")).toBe(true);
    for (const option of options) {
      expect(classes(option).has("border-0")).toBe(true);
      expect(classes(option).has("border-e")).toBe(true);
      expect(classes(option).has("border-e-control-edge")).toBe(true);
      expect(classes(option).has("last:border-e-0")).toBe(true);
      expect(classes(option).has("rounded-none")).toBe(true);
      expect(classes(option).has("leading-normal")).toBe(true);
      expect(classes(option).has("h-auto")).toBe(true);
      expect(classes(option).has("px-[var(--reddb-spatial-inset-md)]")).toBe(true);
    }
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});

describe("the deliberately failing ToggleGroup fixture", () => {
  it("diagnoses a group without single-selection semantics", () => {
    const root = rendered(
      render(RangePressedContractFailures, { failure: "multi-select-toggle-group" }),
    );
    expect(singleSelectionFailures(root)).toEqual([
      "toggle group does not expose exactly one pressed option",
    ]);
  });
});

describe("the ToggleGroup pressed language", () => {
  it("draws the pressed option neutral with a 2px ink bottom indicator, never a red fill", () => {
    const root = rendered(render(ToggleGroup, { legend: "Alignment", options: OPTIONS, value: "center" }));
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")];
    const pressed = buttons.find((button) => button.getAttribute("aria-pressed") === "true")!;

    expect(pressed.textContent?.trim()).toBe("Center");
    expect(classes(pressed).has("font-medium")).toBe(true);
    for (const button of buttons) {
      const names = classes(button);
      expect(names.has("aria-pressed:bg-foreground/10")).toBe(true);
      expect(names.has("aria-pressed:text-foreground")).toBe(true);
      expect(names.has("aria-pressed:border-b-foreground")).toBe(true);
      // The edge is reserved on every option, transparent until pressed.
      expect(names.has("border-b-2")).toBe(true);
      expect(names.has("border-transparent")).toBe(true);
      expect([...names].some((name) => /(?:^|:)bg-primary(?:\/|$)|text-on-primary/.test(name))).toBe(false);
    }
  });
});
