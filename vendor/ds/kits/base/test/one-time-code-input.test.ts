import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import SpecializedEntryConsumer from "./fixtures/SpecializedEntryConsumer.svelte";
import SpecializedEntryContractFailures from "./fixtures/SpecializedEntryContractFailures.svelte";
import {
  OneTimeCodeInput,
  oneTimeCodeInput,
} from "./fixtures/specialized-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function segments(root: HTMLElement): HTMLInputElement[] {
  return [...root.querySelectorAll<HTMLInputElement>("[data-otp-segment]")];
}

function paste(target: HTMLInputElement, text: string): void {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { getData: () => text },
  });
  target.dispatchEvent(event);
  flushSync();
}

function otpPasteFailures(root: HTMLElement): string[] {
  const controls = [...root.querySelectorAll<HTMLInputElement>('input[maxlength="1"]')];
  if (controls.length === 0) return ["OTP has no one-character segments"];
  paste(controls[0]!, "123456");
  return controls.map(({ value }) => value).join("") === "123456"
    ? []
    : ["pasted OTP is not distributed across segments"];
}

describe("the Base OneTimeCodeInput", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(OneTimeCodeInput).toBeDefined();
    expect(oneTimeCodeInput).toBeTypeOf("function");
  });

  it("composes a visibly named native group of one-character inputs", () => {
    const root = rendered(render(OneTimeCodeInput, {
      label: "Verification code",
      name: "code",
      length: 6,
      required: true,
    }));
    const controls = segments(root);

    expect(root.tagName).toBe("FIELDSET");
    expect(root.querySelector(":scope > legend")?.textContent).toBe("Verification code");
    expect(controls).toHaveLength(6);
    expect(controls.every(({ maxLength }) => maxLength === 1)).toBe(true);
    expect(controls.every(({ inputMode }) => inputMode === "numeric")).toBe(true);
    expect(controls.every(({ required }) => required)).toBe(true);
    expect(controls.map((control) => control.getAttribute("aria-label"))).toEqual([
      "Digit 1 of 6", "Digit 2 of 6", "Digit 3 of 6",
      "Digit 4 of 6", "Digit 5 of 6", "Digit 6 of 6",
    ]);
    controls[0]?.focus();
    expect(document.activeElement).toBe(controls[0]);
  });

  it("distributes a pasted code, updates its form value, and reports completion", () => {
    const oncomplete = vi.fn();
    const form = rendered(render(SpecializedEntryConsumer, { otpOncomplete: oncomplete }));
    const root = form.querySelector<HTMLElement>("[data-one-time-code-input]")!;
    const controls = segments(root);

    paste(controls[0]!, "12 34-56");

    expect(controls.map(({ value }) => value).join("")).toBe("123456");
    expect(root.querySelector<HTMLInputElement>('input[type="hidden"]')?.value).toBe("123456");
    expect(new FormData(form as HTMLFormElement).get("verificationCode")).toBe("123456");
    expect(document.activeElement).toBe(controls[5]);
    expect(oncomplete).toHaveBeenCalledWith("123456");
    expect(otpPasteFailures(root)).toEqual([]);
  });

  it("advances on entry and supports Backspace and arrow focus movement", () => {
    const root = rendered(render(OneTimeCodeInput, { label: "Code", length: 4 }));
    const controls = segments(root);

    controls[0]!.value = "7";
    controls[0]!.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(document.activeElement).toBe(controls[1]);

    controls[1]!.dispatchEvent(new KeyboardEvent("keydown", { key: "Backspace", bubbles: true }));
    expect(document.activeElement).toBe(controls[0]);
    controls[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(document.activeElement).toBe(controls[1]);
    controls[1]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    expect(document.activeElement).toBe(controls[0]);
  });

  it("wears token-backed appearance and inherits a nested appearance scope", () => {
    const root = rendered(render(OneTimeCodeInput, { label: "Code", length: 4 }));
    const styles = oneTimeCodeInput();
    const nested = rendered(render(SpecializedEntryConsumer, {}))
      .querySelector<HTMLElement>('[data-appearance-scope] [data-one-time-code-input]')!;

    expect(classes(root)).toEqual(classesOf(styles.root()));
    expect(classes(root.querySelector("[data-otp-list]")!)).toEqual(classesOf(styles.list()));
    expect(classes(segments(root)[0]!)).toEqual(classesOf(styles.segment()));
    expect(classes(segments(root)[0]!).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});

describe("the deliberately failing one-time-code fixture", () => {
  it("diagnoses an OTP control that breaks paste", () => {
    const root = rendered(render(SpecializedEntryContractFailures, {
      failure: "broken-otp-paste",
    }));
    expect(otpPasteFailures(root)).toEqual([
      "pasted OTP is not distributed across segments",
    ]);
  });
});
