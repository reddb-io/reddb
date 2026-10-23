import { describe, expect, it } from "vitest";
import CommandSurfaceContractFailures from "./fixtures/CommandSurfaceContractFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing application command surfaces", () => {
  it("diagnoses a context menu that cannot be reached without a pointer", () => {
    const root = render(CommandSurfaceContractFailures, {
      failure: "pointer-only-context-menu",
    });
    const target = root.querySelector<HTMLElement>("[data-context-target]")!;

    expect(target.tabIndex).toBe(-1);
    expect(target.querySelector("button")).toBeNull();
    expect(target.getAttribute("aria-haspopup")).toBeNull();
  });

  it("diagnoses a toolbar without one roving tab stop", () => {
    const root = render(CommandSurfaceContractFailures, {
      failure: "toolbar-without-roving-focus",
    });
    const toolbar = root.querySelector<HTMLElement>('[role="toolbar"]')!;
    const tabbable = [...toolbar.querySelectorAll<HTMLElement>("button")].filter(
      (control) => control.tabIndex === 0,
    );

    expect(tabbable).toHaveLength(3);
  });
});
