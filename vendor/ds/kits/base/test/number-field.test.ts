import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import NumberField from "../src/NumberField.svelte";
import Consumer from "./fixtures/NumberFieldConsumer.svelte";
import { render } from "./mount";

function edit(input:HTMLInputElement,text:string) { input.value=text;input.dispatchEvent(new Event("input",{bubbles:true}));flushSync(); }
describe("NumberField's native numeric contract",()=>{
  it("steps decimals exactly once, retains native bounds and associates its help",()=>{
    const changed=vi.fn();const root=render(NumberField,{label:"Rate",value:0.3,min:0,max:0.5,step:0.1,help:"Sampling fraction",onvaluechange:changed});
    const input=root.querySelector("input")!;const buttons=root.querySelectorAll("button");
    buttons[1]!.click();flushSync();expect(input.valueAsNumber).toBe(0.4);expect(changed).toHaveBeenLastCalledWith(0.4);
    buttons[1]!.click();flushSync();expect(input.valueAsNumber).toBe(0.5);expect(buttons[1]!.disabled).toBe(true);expect(changed).toHaveBeenCalledTimes(2);
    buttons[1]!.click();expect(changed).toHaveBeenCalledTimes(2);
    expect(root.querySelector(`label[for="${input.id}"]`)?.textContent).toContain("Rate");
    expect(document.getElementById(input.getAttribute("aria-describedby")!)?.textContent).toBe("Sampling fraction");
  });
  it("retains invalid typed values for native validity and submits through FormData",()=>{
    const root=render(Consumer);const input=root.querySelector("input")!;edit(input,"20");
    expect(input.validity.rangeOverflow).toBe(true);expect(input.value).toBe("20");
    expect(new FormData(root.querySelector("form")!).get("units")).toBe("20");
    expect(root.querySelector("[data-bound-value]")?.textContent).toBe("20");
  });
  it("synchronizes cleared controlled values and native form resets",async()=>{
    const root=render(Consumer);const input=root.querySelector("input")!;edit(input,"7");
    [...root.querySelectorAll<HTMLButtonElement>('button')].find(button=>button.textContent?.trim()==="Clear externally")!.click();flushSync();
    expect(root.querySelector("[data-bound-value]")?.textContent).toBe("empty");expect(input.value).toBe("");
    root.querySelector("form")!.reset();await new Promise<void>(resolve=>setTimeout(resolve,0));await tick();flushSync();
    expect(input.value).toBe("3");expect(root.querySelector("[data-bound-value]")?.textContent).toBe("3");
  });
  it("does not change a readonly or disabled value",()=>{
    for(const props of [{readonly:true},{disabled:true}]) {
      const changed=vi.fn();const root=render(NumberField,{label:"Managed",value:3,onvaluechange:changed,...props});
      for(const button of root.querySelectorAll("button")){expect(button.disabled).toBe(true);button.click();}
      expect(changed).not.toHaveBeenCalled();expect(root.querySelector("input")!.value).toBe("3");
    }
  });
});
