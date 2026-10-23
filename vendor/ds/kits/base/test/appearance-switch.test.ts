import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import SurfaceControlsContractFailures from "./fixtures/SurfaceControlsContractFailures.svelte";
import {
  APPEARANCE_AXES,
  AppearanceSwitch,
  appearanceAttribute,
  appearanceStorageKey,
} from "./fixtures/surface-controls-consumer";
import { render, rendered } from "./mount";

const OPTIONS = [
  { value: "application", label: "Application" },
  { value: "marketing", label: "Marketing" },
] as const;

function memoryStorage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem: vi.fn((key: string) => values.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => values.set(key, value)),
  };
}

describe("the deliberately unpersisted appearance-switch fixture", () => {
  it("demonstrates an axis change that disappears outside the DOM", () => {
    const key = "reddb:test:broken-theme";
    localStorage.removeItem(key);
    const select = rendered(
      render(SurfaceControlsContractFailures, {
        failure: "unpersisted-appearance-switch",
        storageKey: key,
      }),
    ).querySelector<HTMLSelectElement>("select")!;

    select.value = "marketing";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    expect(document.documentElement.getAttribute("data-theme")).toBe("marketing");
    expect(localStorage.getItem(key)).toBeNull();
  });
});

describe("the Base AppearanceSwitch", () => {
  it("keeps all appearance axes available through one generic contract", () => {
    expect(APPEARANCE_AXES).toEqual(["theme", "color-scheme", "density"]);
    expect(APPEARANCE_AXES.map(appearanceAttribute)).toEqual([
      "data-theme",
      "data-color-scheme",
      "data-density",
    ]);
    expect(appearanceStorageKey("theme")).not.toBe(appearanceStorageKey("density"));
  });

  it("composes a visibly named native Select with keyboard focus", () => {
    const control = rendered(
      render(AppearanceSwitch, {
        label: "Theme",
        axis: "theme",
        options: OPTIONS,
        value: "application",
        storage: memoryStorage(),
      }),
    ).querySelector<HTMLSelectElement>("select")!;

    expect(control.tagName).toBe("SELECT");
    expect(control.labels?.[0]?.textContent?.trim()).toBe("Theme");
    expect([...control.options].map(({ value, textContent }) => [value, textContent])).toEqual([
      ["application", "Application"],
      ["marketing", "Marketing"],
    ]);
    control.focus();
    expect(document.activeElement).toBe(control);
  });

  it("persists only the selected axis while leaving the others free", () => {
    const storage = memoryStorage();
    const onvaluechange = vi.fn();
    const root = document.createElement("div");
    root.setAttribute("data-color-scheme", "dark");
    root.setAttribute("data-density", "compact");
    const control = rendered(
      render(AppearanceSwitch, {
        label: "Theme",
        axis: "theme",
        options: OPTIONS,
        value: "application",
        root,
        storage,
        persistKey: "reddb:test:theme",
        onvaluechange,
      }),
    ).querySelector<HTMLSelectElement>("select")!;

    control.value = "marketing";
    control.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    expect(root.getAttribute("data-theme")).toBe("marketing");
    expect(root.getAttribute("data-color-scheme")).toBe("dark");
    expect(root.getAttribute("data-density")).toBe("compact");
    expect(storage.setItem).toHaveBeenLastCalledWith("reddb:test:theme", "marketing");
    expect(onvaluechange).toHaveBeenCalledWith("marketing");
  });

  it("restores a valid persisted value on mount", () => {
    const root = document.createElement("div");
    const storage = memoryStorage({ "reddb:test:theme": "marketing" });
    const control = rendered(
      render(AppearanceSwitch, {
        label: "Theme",
        axis: "theme",
        options: OPTIONS,
        value: "application",
        root,
        storage,
        persistKey: "reddb:test:theme",
      }),
    ).querySelector<HTMLSelectElement>("select")!;
    flushSync();

    expect(control.value).toBe("marketing");
    expect(root.getAttribute("data-theme")).toBe("marketing");
  });
});
