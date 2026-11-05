import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import RangePressedConsumer from "./fixtures/RangePressedConsumer.svelte";
import { ToggleButton, toggleButton } from "./fixtures/range-pressed-consumer";
import { button } from "../src/button.variants";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base ToggleButton", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(ToggleButton).toBeDefined();
    expect(toggleButton).toBeTypeOf("function");
  });

  it("composes the canonical Button with explicit pressed semantics", () => {
    const element = rendered(render(ToggleButton, { label: "Pin deployment", pressed: true }));

    expect(element.tagName).toBe("BUTTON");
    expect(element.getAttribute("type")).toBe("button");
    expect(element.getAttribute("aria-pressed")).toBe("true");
    expect(element.getAttribute("data-state")).toBe("on");
    expect(element.textContent?.trim()).toBe("Pin deployment");
    expect(classes(element)).toEqual(
      classesOf(button({ variant: "secondary", class: toggleButton({ pressed: true }) })),
    );
  });

  it("toggles through native activation and still calls the consumer", () => {
    const onclick = vi.fn();
    const element = rendered(render(ToggleButton, { label: "Pin deployment", onclick }));

    expect(element.getAttribute("aria-pressed")).toBe("false");
    element.click();
    flushSync();
    expect(element.getAttribute("aria-pressed")).toBe("true");
    expect(element.getAttribute("data-state")).toBe("on");
    expect(onclick).toHaveBeenCalledTimes(1);

    element.click();
    flushSync();
    expect(element.getAttribute("aria-pressed")).toBe("false");
  });

  it("keeps native focus and forwards keyboard events", () => {
    const onkeydown = vi.fn();
    const element = rendered(render(ToggleButton, { label: "Pin", onkeydown }));
    element.focus();
    expect(document.activeElement).toBe(element);
    element.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    flushSync();
    expect(onkeydown).toHaveBeenCalledTimes(1);
  });

  it("does not change while disabled", () => {
    const element = rendered(render(ToggleButton, { label: "Pin", disabled: true }));
    element.click();
    flushSync();
    expect(element.getAttribute("aria-pressed")).toBe("false");
  });

  it("keeps appearance axes caller-owned in a nested scope", () => {
    const form = rendered(render(RangePressedConsumer, {}));
    const element = form.querySelector<HTMLElement>("[data-appearance-scope] [data-toggle-button]")!;

    expect(classes(element).has("aria-pressed:bg-foreground/10")).toBe(true);
    expect(classes(element).has("aria-pressed:border-b-foreground")).toBe(true);
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
