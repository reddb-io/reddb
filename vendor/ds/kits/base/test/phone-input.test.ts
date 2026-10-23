import { act, createElement as h } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync } from "svelte";
import SveltePhoneInput from "../src/PhoneInput.svelte";
import { PhoneInput as ReactPhoneInput } from "../react/src/PhoneInput";
import { render } from "./mount";
import { bindPhoneEditing, phoneCountries, phoneCountryOptions, phoneProblem, phoneSeparatorDeletion, phoneState } from "../src/core/phone-input";
import type { PhoneInputContract } from "../src/core/contracts/phoneinput";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let react: Root | undefined;
afterEach(async () => { if (react) await act(() => react?.unmount()); react = undefined; document.querySelectorAll("form[data-phone-test]").forEach(node => node.remove()); });
describe("portable country-aware phone contract", () => {
  it.each([
    ["BR", "11999991234", "(11) 99999-1234", "+5511999991234"],
    ["BR", "1133334444", "(11) 3333-4444", "+551133334444"],
    ["US", "2025550123", "(202) 555-0123", "+12025550123"],
    ["PT", "912345678", "912 345 678", "+351912345678"],
    ["GB", "02079460958", "020 7946 0958", "+442079460958"],
    ["BR", "１１９９９９９１２３４", "(11) 99999-1234", "+5511999991234"],
  ] as const)("formats %s without leaking mask or trunk prefixes to the value", (country, raw, display, value) => {
    expect(phoneState(raw, country)).toMatchObject({ country, display, value, possible: true });
    expect(phoneState(value, country).value).toBe(value);
  });
  it("infers international paste, rejects incomplete/long/extension numbers and respects restrictions", () => {
    expect(phoneState("+12025550123", "BR")).toMatchObject({ country: "US", display: "(202) 555-0123", value: "+12025550123", possible: true });
    expect(phoneState("+442079460958", "US").international).toBe("+44 20 7946 0958");
    for (const text of ["+", "+5", "+9991234", "11999", "119999912341234567890", "11999991234 ext 123"]) {
      const state = phoneState(text, "BR");
      expect(state.possible, text).toBe(false);
      expect(phoneProblem(state, ["BR"], false, "Required", "Invalid"), text).toBe("Invalid");
    }
    expect(phoneProblem(phoneState("", "BR"), ["BR"], false, "Required", "Invalid")).toBe("");
    expect(phoneProblem(phoneState("", "BR"), ["BR"], true, "Required", "Invalid")).toBe("Required");
    expect(phoneProblem(phoneState("+12025550123", "BR"), ["BR"], true, "Required", "Invalid")).toBe("Invalid");
    expect(() => phoneCountries([])).toThrow(RangeError);
    const options = phoneCountryOptions(["BR", "US"], "pt-BR");
    expect(options.find(option => option.value === "BR")).toMatchObject({ label: "🇧🇷 +55", description: "Brasil" });
    expect(options.find(option => option.value === "US")?.keywords).toContain("United States");
  });
  it("preserves selection, deletes digits across separators and waits for IME composition", () => {
    const node = document.createElement("input"); node.type = "tel";
    const changed = vi.fn(); const binding = bindPhoneEditing(node, phoneState("11999991234", "BR"), changed);
    node.value = "(11) 99999-1234"; node.setSelectionRange(5, 5);
    const before = new InputEvent("beforeinput", { inputType: "deleteContentBackward", cancelable: true }); node.dispatchEvent(before);
    expect(before.defaultPrevented).toBe(true); expect(changed.mock.lastCall?.[0].value).toBe("+551999991234");
    expect(node.selectionStart).toBe(2);
    expect(phoneSeparatorDeletion("(11) 99999-1234", "deleteContentForward", 3, 3)).toEqual({ text: "1199991234", digits: 2 });
    changed.mockClear(); node.dispatchEvent(new CompositionEvent("compositionstart")); node.value = "１１９９９９９１２３４"; node.dispatchEvent(new InputEvent("input", { isComposing: true }));
    expect(changed).not.toHaveBeenCalled(); expect(node.value).toBe("１１９９９９９１２３４");
    node.dispatchEvent(new CompositionEvent("compositionend")); expect(changed.mock.lastCall?.[0].value).toBe("+5511999991234");
    binding.destroy();
  });
});

for (const flavor of ["Svelte", "React"] as const) describe(`${flavor} phone forms`, () => {
  async function mount(props: PhoneInputContract, changed = vi.fn()) {
    const form = document.createElement("form"); form.id = "phone-test"; form.dataset.phoneTest = ""; document.body.append(form);
    let root: HTMLElement;
    if (flavor === "Svelte") root = render(SveltePhoneInput, { ...props, form: form.id, onvaluechange: changed });
    else { root = document.createElement("div"); document.body.append(root); await act(() => { react = createRoot(root); react.render(h(ReactPhoneInput, { ...props, form: form.id, onValueChange: changed })); }); }
    const input = root.querySelector<HTMLInputElement>('input[type="tel"]')!;
    async function edit(text: string) { await act(() => { input.value = text; input.setSelectionRange(text.length, text.length); input.dispatchEvent(new InputEvent("input", { bubbles: true })); flushSync(); }); }
    return { form, root, input, edit, changed };
  }
  it("shows a national mask but submits exactly one unmasked international value", async () => {
    const { form, root, input, edit, changed } = await mount({ label: "Phone", name: "phone", defaultCountry: "BR", defaultValue: "+5511999991234", help: "Contact details", required: true });
    expect(input.value).toBe("(11) 99999-1234"); expect(input.name).toBe("");
    expect(new FormData(form).getAll("phone")).toEqual(["+5511999991234"]);
    expect(document.getElementById(input.getAttribute("aria-describedby")!)?.textContent).toBe("Contact details");
    await edit("+12025550123"); expect(input.value).toBe("(202) 555-0123"); expect(changed).toHaveBeenLastCalledWith("+12025550123");
    expect(root.querySelector('[data-search-select-trigger]')?.getAttribute("aria-label")).toContain("United States");
    expect(new FormData(form).getAll("phone")).toEqual(["+12025550123"]); expect(input.checkValidity()).toBe(true);
  });
  it("blocks incomplete/restricted numbers, keeps canonical drafts and resets an externally owned form", async () => {
    const { form, input, edit, changed } = await mount({ label: "Phone", name: "phone", defaultCountry: "BR", countries: ["BR", "PT"], defaultValue: "+5511999991234", required: true });
    await edit("11999"); expect(input.checkValidity()).toBe(false); expect(new FormData(form).get("phone")).toBe("+5511999");
    await edit("+12025550123"); expect(input.checkValidity()).toBe(false); expect(new FormData(form).get("phone")).toBe("+12025550123");
    await edit(""); expect(new FormData(form).get("phone")).toBe(""); expect(input.checkValidity()).toBe(false);
    await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); });
    expect(input.value).toBe("(11) 99999-1234"); expect(new FormData(form).get("phone")).toBe("+5511999991234"); expect(changed).toHaveBeenLastCalledWith("+5511999991234");
  });
  it("renders saved phones as selectable text and omits disabled fields from submission", async () => {
    const saved = await mount({ label: "Saved phone", name: "phone", value: "+5511999991234", presentation: "text", sourceLabel: "Contacts API" });
    expect(saved.root.querySelector('input:not([type="hidden"])')).toBeNull(); expect(saved.root.querySelector("button")).toBeNull();
    expect(saved.root.querySelector("[data-phone-value]")?.textContent).toContain("+55 11 99999 1234");
    expect(new FormData(saved.form).get("phone")).toBe("+5511999991234");
  });
  it("disables both editing controls and omits its named field", async () => {
    const { form, root, input, changed } = await mount({ label: "Disabled phone", name: "phone", defaultValue: "+5511999991234", disabled: true });
    expect(input.disabled).toBe(true); expect(root.querySelector<HTMLButtonElement>("button")?.disabled).toBe(true);
    expect(new FormData(form).has("phone")).toBe(false); expect(changed).not.toHaveBeenCalled();
  });
});
