import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import DialogTooltipBehaviorFailures from "./fixtures/DialogTooltipBehaviorFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing Dialog and Tooltip fixtures", () => {
  it("diagnoses a Dialog without an accessible label", () => {
    const root = render(DialogTooltipBehaviorFailures, { failure: "missing-label" });
    const dialog = root.querySelector('[role="dialog"]')!;
    const named = dialog.hasAttribute("aria-label") || dialog.hasAttribute("aria-labelledby");
    expect(named ? [] : ["dialog has no accessible label"]).toEqual([
      "dialog has no accessible label",
    ]);
  });

  it("diagnoses a Dialog that ignores Escape", () => {
    const root = render(DialogTooltipBehaviorFailures, { failure: "ignored-escape" });
    root.querySelector<HTMLButtonElement>("[data-opener]")!.click();
    flushSync();
    root
      .querySelector<HTMLElement>('[role="dialog"]')!
      .dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    flushSync();
    expect(root.querySelector("[data-broken-dialog]") ? ["Escape did not dismiss"] : []).toEqual([
      "Escape did not dismiss",
    ]);
  });

  it("diagnoses a Dialog that loses the invoking focus", () => {
    const root = render(DialogTooltipBehaviorFailures, { failure: "lost-focus" });
    const opener = root.querySelector<HTMLButtonElement>("[data-opener]")!;
    opener.focus();
    opener.click();
    flushSync();
    const close = root.querySelector<HTMLButtonElement>("[data-close]")!;
    close.focus();
    close.click();
    flushSync();
    expect(document.activeElement === opener ? [] : ["focus was not restored"]).toEqual([
      "focus was not restored",
    ]);
  });

  it("diagnoses a pointer-only Tooltip trigger", () => {
    const root = render(DialogTooltipBehaviorFailures, { failure: "pointer-only-tooltip" });
    root.querySelector<HTMLButtonElement>("[data-tooltip-trigger]")!.focus();
    flushSync();
    expect(root.querySelector('[role="tooltip"]') ? [] : ["focus did not open tooltip"]).toEqual([
      "focus did not open tooltip",
    ]);
  });
});
