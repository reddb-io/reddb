import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import DrawerAlertDialogBehaviorFailures from "./fixtures/DrawerAlertDialogBehaviorFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing Drawer and AlertDialog fixtures", () => {
  it("diagnoses a Drawer that loses the invoking focus", () => {
    const root = render(DrawerAlertDialogBehaviorFailures, { failure: "drawer-lost-focus" });
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

  it("diagnoses an AlertDialog dismissed by an outside click", () => {
    const root = render(DrawerAlertDialogBehaviorFailures, {
      failure: "alert-outside-dismissal",
    });
    root.querySelector<HTMLButtonElement>("[data-opener]")!.click();
    flushSync();
    root.querySelector<HTMLElement>("[data-broken-alert-dialog]")!.click();
    flushSync();

    expect(root.querySelector("[data-broken-alert-dialog]") ? [] : ["outside click dismissed"]).toEqual([
      "outside click dismissed",
    ]);
  });
});
