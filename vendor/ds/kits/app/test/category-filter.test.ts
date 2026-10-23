import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import CategoryFilter from "../src/composites/CategoryFilter.svelte";
import { categoryFilter } from "../src/composites/category-filter.variants";
import { classes, classesOf, render, rendered } from "./mount";

const OPTIONS = [
  { value: "shirts", label: "Shirts" },
  { value: "shoes", label: "Shoes" },
  { value: "accessories", label: "Accessories" },
] as const;

describe("CategoryFilter", () => {
  it("composes a named set of native checkboxes whose active categories are announced", () => {
    const root = rendered(
      render(CategoryFilter, {
        legend: "Categories",
        options: OPTIONS,
        value: ["shirts", "accessories"],
        name: "category",
      }),
    );
    const controls = [...root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Categories");
    expect(controls.map((control) => control.checked)).toEqual([true, false, true]);
    expect(controls.map((control) => control.name)).toEqual([
      "category",
      "category",
      "category",
    ]);
    expect(controls.map((control) => control.value)).toEqual([
      "shirts",
      "shoes",
      "accessories",
    ]);
    expect(controls.map((control) => root.querySelector(`label[for="${control.id}"]`)?.textContent?.trim()))
      .toEqual(["Shirts", "Shoes", "Accessories"]);
  });

  it("keeps every category keyboard reachable and publishes changed active values", () => {
    const onvaluechange = vi.fn();
    const root = rendered(
      render(CategoryFilter, {
        legend: "Categories",
        options: OPTIONS,
        value: ["shirts"],
        onvaluechange,
      }),
    );
    const controls = [...root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];

    controls[1]!.focus();
    expect(document.activeElement).toBe(controls[1]);
    controls[1]!.click();
    flushSync();

    expect(controls[1]!.checked).toBe(true);
    expect(onvaluechange).toHaveBeenLastCalledWith(["shirts", "shoes"]);
  });

  it("keeps Density tokenized and inherits every nested appearance axis", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const target = render(CategoryFilter, {
      legend: "Categories",
      options: OPTIONS,
      value: [],
    });
    scope.append(...target.children);
    const surface = scope.querySelector<HTMLElement>("[data-category-filter]")!;
    const list = surface.querySelector<HTMLElement>("[data-category-filter-list]")!;
    const styles = categoryFilter();

    for (const name of classesOf(styles.root())) expect(classes(surface)).toContain(name);
    expect(classes(list)).toContain("gap-[var(--reddb-spatial-gap-sm)]");
    expect(surface.hasAttribute("data-theme")).toBe(false);
    expect(surface.hasAttribute("data-color-scheme")).toBe(false);
    expect(surface.hasAttribute("data-density")).toBe(false);
  });
});
