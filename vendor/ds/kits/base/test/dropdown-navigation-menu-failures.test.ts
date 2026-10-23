import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import MenuBehaviorFailures from "./fixtures/MenuBehaviorFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing menu fixtures", () => {
  it("diagnoses a menu without one roving tab stop", () => {
    const root = render(MenuBehaviorFailures, { failure: "no-roving-focus" });
    const rows = [...root.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    rows[0]!.focus();
    rows[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    flushSync();

    const tabbable = rows.filter((row) => row.tabIndex === 0);
    expect(tabbable).toHaveLength(2);
    expect(document.activeElement).toBe(rows[0]);
  });

  it("diagnoses a menu that survives Escape", () => {
    const root = render(MenuBehaviorFailures, { failure: "escape-survives" });
    const row = root.querySelector<HTMLElement>('[role="menuitem"]')!;
    row.focus();
    row.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    flushSync();

    expect(root.querySelector("[data-broken-menu]")).not.toBeNull();
    expect(root.querySelector('[aria-expanded="true"]')).not.toBeNull();
  });
});
