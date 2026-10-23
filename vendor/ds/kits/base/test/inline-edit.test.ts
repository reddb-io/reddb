import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import InlineEdit from "../src/InlineEdit.svelte";
import { render } from "./mount";

async function edit(root:HTMLElement) { root.querySelector<HTMLButtonElement>("button")!.click(); flushSync(); await tick(); return root.querySelector<HTMLInputElement>("input")!; }
function type(input:HTMLInputElement,value:string) { input.value=value;input.dispatchEvent(new Event("input",{bubbles:true}));flushSync(); }
async function submit(root:HTMLElement) { root.querySelector("form")!.dispatchEvent(new SubmitEvent("submit",{bubbles:true,cancelable:true}));await tick();flushSync();await tick(); }

describe("InlineEdit committed and draft state",()=>{
  it("presents saved API values as plain text without a focusable input until explicitly unlocked",async()=>{
    const changed=vi.fn();const root=render(InlineEdit,{label:"API name",value:"From API",presentation:"input",sourceLabel:"Configuration API",onvaluechange:changed});
    expect(root.querySelector("input")).toBeNull();
    expect(root.querySelector("[data-inline-edit-value]")?.textContent?.trim()).toBe("From API");
    expect(root.querySelector("[data-inline-edit-value]")?.getAttribute("tabindex")).toBeNull();
    expect(root.querySelector("[data-saved-value]")?.textContent).toContain("Configuration API");
    const input=await edit(root);expect(input.readOnly).toBe(false);type(input,"Updated API value");await submit(root);
    expect(changed).toHaveBeenCalledExactlyOnceWith("Updated API value");
    expect(root.querySelector("input")).toBeNull();
    expect(root.querySelector("[data-inline-edit-value]")?.textContent?.trim()).toBe("Updated API value");
  });
  it("never renders the saved password in the read DOM",async()=>{
    const root=render(InlineEdit,{label:"Secret",value:"private-api-secret",inputType:"password",presentation:"input"});
    expect(root.innerHTML).not.toContain("private-api-secret");
    expect(root.querySelector("[data-inline-edit-value]")?.textContent?.trim()).toBe("••••••••");
    expect((await edit(root)).type).toBe("password");
  });
  it("does not expose draft changes as committed values and restores the edit trigger after save",async()=>{
    const changed=vi.fn();const root=render(InlineEdit,{label:"Project name",value:"Original",onvaluechange:changed});
    const input=await edit(root);expect(document.activeElement).toBe(input);type(input,"Updated");expect(changed).not.toHaveBeenCalled();
    await submit(root);expect(changed).toHaveBeenCalledExactlyOnceWith("Updated");expect(root.textContent).toContain("Updated");
    expect(document.activeElement).toBe(root.querySelector("button"));
  });
  it("Escape discards the draft and reopens from the committed value",async()=>{
    const changed=vi.fn();const root=render(InlineEdit,{label:"Name",value:"Original",onvaluechange:changed});
    const input=await edit(root);type(input,"Discarded");input.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true,cancelable:true}));await tick();flushSync();
    expect(changed).not.toHaveBeenCalled();expect((await edit(root)).value).toBe("Original");
  });
  it("retains invalid and rejected drafts, associates the error, and retries without data loss",async()=>{
    const save=vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(undefined),changed=vi.fn();
    const root=render(InlineEdit,{label:"Name",value:"Original",onconfirm:save,onvaluechange:changed});
    const input=await edit(root);type(input,"   ");await submit(root);expect(save).not.toHaveBeenCalled();expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(root.querySelector("[role=alert]")!.id).toBe(input.getAttribute("aria-describedby"));
    type(input,"Retry me");await submit(root);expect(input.value).toBe("Retry me");expect(changed).not.toHaveBeenCalled();expect(document.activeElement).toBe(input);
    await submit(root);expect(changed).toHaveBeenCalledExactlyOnceWith("Retry me");
  });
  it("blocks duplicate persistence while a request is pending",async()=>{
    let finish!:()=>void;const save=vi.fn(()=>new Promise<void>(resolve=>finish=resolve));
    const root=render(InlineEdit,{label:"Name",value:"A",onconfirm:save});const input=await edit(root);type(input,"B");await submit(root);await submit(root);
    expect(save).toHaveBeenCalledTimes(1);expect(input.disabled).toBe(true);finish();await tick();flushSync();
  });
  it("keeps composition Enter inside the input instead of submitting the draft",async()=>{
    const root=render(InlineEdit,{label:"Name",value:"A"});const input=await edit(root);
    const event=new KeyboardEvent("keydown",{key:"Enter",isComposing:true,bubbles:true,cancelable:true});input.dispatchEvent(event);expect(event.defaultPrevented).toBe(true);
  });
});
